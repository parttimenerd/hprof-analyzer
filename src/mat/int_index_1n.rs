//! Writers for MAT `IntArray1N` (int-array 1-to-N) index files.
//!
//! Mirrors `IndexWriter.IntArray1NWriter` and `IntArray1NSortedWriter` from
//! Eclipse MAT 1.13.0. An IntArray1N file maps each object `index` (0..count)
//! to a variable-length `int[]` of related object ids (e.g. the outbound
//! references of an object, or the objects dominated by a dominator).
//!
//! See `docs/mat-cache.md` for the overall design and RSS budget discussion.
//! The memory strategy for [`write_sorted_cb`] (zstd-compressed header spool)
//! is documented in that file under "zstd-compressed header spool".
//!
//! File layout (all big-endian):
//! ```text
//!   [body pages] ++ [body footer]              // an IntIndexStreamer @ offset 0
//!   [header pages] ++ [header footer]          // an IntIndexStreamer @ offset `divider`
//!   [divider : i64]                            // trailing 8 bytes
//! ```
//!
//! * The **body** is one big int stream holding every entry's values (plus, in
//!   the unsorted layout, a length int per entry). It is an [`IntIndexStreamer`]
//!   started at file offset 0; its footer sits at the end of the body region.
//! * The **divider** = `body.closeStream()` = the total byte length of the body
//!   region (because the body starts at 0 the close value equals
//!   `body_bytes.len()`). It is the offset at which the header index begins and
//!   is *also* written as the final 8 bytes of the file.
//! * The **header** index stores one int per object: the position, within the
//!   body value stream, of that object's entry. It is an [`IntIndexStreamer`]
//!   opened at position `divider`, so its `pageStart[0] == divider`.
//!
//! Two layouts differ only in how the body is packed and what the header
//! positions mean:
//!
//! * **Unsorted** ([`write_unsorted`], MAT `IntArray1NWriter`): the body is
//!   `[len0, v0.., len1, v1.., ...]`. `header[i]` is the value-index of entry
//!   `i`'s length int. `add(index, values)` does:
//!   ```text
//!   long pos = body.size;   // value-count before writing
//!   body.add(values.length);
//!   body.addAll(values);
//!   header.set(index, pos);
//!   ```
//! * **Sorted** ([`write_sorted`], MAT `IntArray1NSortedWriter`): the body is
//!   just `[v0.., v1.., ...]` with no length ints. `header[i]` is **1-based**
//!   and delimits entries: entry `i` = `body[header[i]-1 .. header[i+1]-1]`.
//!   `set(index, values)` does:
//!   ```text
//!   long pos = body.size + 1;   // 1-based
//!   header.set(index, pos);
//!   body.addAll(values);
//!   ```
//!   The final delimiter (`body.size + 1` after the last entry) is what the
//!   reader uses as the end of the last entry.
//!
//!   MAT only calls `set` for indices that actually have data; an index that is
//!   never `set` keeps `header[index] == 0` and contributes nothing to the body.
//!   The reader treats a `0` header as an empty entry (`int[0]`). To reproduce a
//!   real file byte-for-byte we therefore map an **empty** `entries[i]` to an
//!   *unset* header (0, no body bytes) rather than emitting `body.size + 1`.
//!   (Verified against the real `inbound.index`, which has 1643 such holes and
//!   no genuinely set-but-empty entries.) The reader's length rule is
//!   `p1 = header[i+1]`, scanning forward past any `0`/smaller headers for the
//!   next `>= p0`, falling back to `body.size + 1` for the final entry.
//!
//! ## header2 / 2^40 limitation
//!
//! MAT keeps a parallel `byte[] header2` with `header2[i] = (byte)(pos >> 32)`
//! to extend positions to 2^40. When every position fits in 32 bits (all
//! `header2` bytes zero) MAT writes the header as a plain [`IntIndexStreamer`];
//! otherwise it switches to a `PosIndexStreamer`. Only the plain-int path is
//! implemented here (our reference dumps have bodies < 4 GiB). If any position
//! reaches 2^32 we return an [`io::Error`] rather than emit a wrong file.

use std::io::{self, BufReader, Read, Write};

use super::int_index::IntIndexStreamer;
#[cfg(test)]
use super::int_index::PAGE_SIZE_INT;

/// Number of bytes an [`IntIndexStreamer`] footer occupies for a value stream
/// of `size` ints: `(pages + 1)` i64 pageStart entries + pageSize:i32 + size:i32.
#[cfg(test)]
fn int_footer_len(size: usize) -> usize {
    let pages = size.div_ceil(PAGE_SIZE_INT).max(1);
    (pages + 1) * 8 + 4 + 4
}

/// Write an unsorted IntArray1N file (MAT `IntArray1NWriter`).
///
/// `entries[i]` is the raw `int[]` for object `i`. Values are laid out in the
/// body as `[len, values...]` per entry and the header records the value-index
/// of each entry's length int. See the module docs for the full framing.
#[allow(dead_code)]
pub fn write_unsorted<W: Write>(mut w: W, entries: &[Vec<i32>]) -> io::Result<W> {
    // --- 1. Build the body value stream in memory, recording header positions.
    // We buffer the body into a Vec first so we can compute `divider` (its total
    // encoded length) before opening the header stream at that offset.
    let mut header: Vec<i64> = Vec::with_capacity(entries.len());
    let mut body = IntIndexStreamer::new(Vec::new());
    let mut body_size: i64 = 0; // value count pushed so far == body.size
    for values in entries {
        let pos = body_size; // header value = size BEFORE writing len
        header.push(pos);
        // add(length) then addAll(values)
        body.push(values.len() as i32)?;
        body_size += 1;
        for &v in values {
            body.push(v)?;
            body_size += 1;
        }
    }
    let body_bytes = body.finish()?; // flushes final page + body footer
    let divider = body_bytes.len() as i64; // body starts at 0 => close value == len

    write_1n_tail(&mut w, &body_bytes, &header, divider)?;
    Ok(w)
}

/// Write a sorted IntArray1N file (MAT `IntArray1NSortedWriter`).
///
/// `entries[i]` is the pre-built (sorted/deduped upstream) `int[]` for object
/// `i`. The body is the concatenation of all values with no length prefixes;
/// header positions are 1-based delimiters. See the module docs.
#[allow(dead_code)]
pub fn write_sorted<W: Write>(mut w: W, entries: &[Vec<i32>]) -> io::Result<W> {
    let mut header: Vec<i64> = Vec::with_capacity(entries.len());
    let mut body = IntIndexStreamer::new(Vec::new());
    let mut body_size: i64 = 0;
    for values in entries {
        if values.is_empty() {
            // Unset entry: MAT never calls set() for a hole, leaving header 0
            // and adding nothing to the body. Emitting `body_size + 1` here
            // would diverge from real files.
            header.push(0);
            continue;
        }
        let pos = body_size + 1; // 1-based, no length prefix
        header.push(pos);
        for &v in values {
            body.push(v)?;
            body_size += 1;
        }
    }
    let body_bytes = body.finish()?;
    let divider = body_bytes.len() as i64;

    write_1n_tail(&mut w, &body_bytes, &header, divider)?;
    Ok(w)
}

/// Wraps a `Write` and counts the total bytes written through it.
struct CountingWriter<W: Write> {
    inner: W,
    count: u64,
}
impl<W: Write> CountingWriter<W> {
    fn new(w: W) -> Self {
        Self { inner: w, count: 0 }
    }
    fn bytes_written(&self) -> u64 {
        self.count
    }
    fn into_inner(self) -> W {
        self.inner
    }
}
impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Low-memory streaming variant of [`write_sorted`]: streams body pages directly
/// to `w` rather than buffering them in a `Vec<u8>`. Holds only the header
/// `Vec<i32>` (~4 bytes/object) in memory instead of the full body bytes
/// (~13 bytes/object for typical Java heap outbound/inbound files).
pub fn write_sorted_iter_streaming<W, I, S>(w: W, entries: I) -> io::Result<W>
where
    W: Write,
    I: Iterator<Item = S>,
    S: AsRef<[i32]>,
{
    let mut cw = CountingWriter::new(w);
    let mut header: Vec<i32> = Vec::new();
    let mut body = IntIndexStreamer::new(&mut cw);
    let mut body_size: i64 = 0;
    for entry in entries {
        let values = entry.as_ref();
        if values.is_empty() {
            header.push(0);
            continue;
        }
        let pos = body_size + 1;
        if pos >= (1i64 << 32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "header2/PosIndexStreamer path not implemented; bodyPos {pos} at entry {}",
                    header.len()
                ),
            ));
        }
        header.push(pos as i32);
        for &v in values {
            body.push(v)?;
            body_size += 1;
        }
    }
    body.finish()?; // flushes final page + body footer directly to `cw`
    let divider = cw.bytes_written() as i64;
    let mut w = cw.into_inner();

    // Write header index opened at `divider`, then trailing divider.
    let mut hdr = IntIndexStreamer::with_position(&mut w, divider);
    for &pos in &header {
        hdr.push(pos)?;
    }
    hdr.finish()?;
    w.write_all(&divider.to_be_bytes())?;
    Ok(w)
}

/// Callback-driven variant of [`write_sorted_iter_streaming`] that avoids
/// per-entry `Vec` allocations.
///
/// `n_entries` times, calls `f(push)` where `push` is a `&mut dyn FnMut(i32)
/// -> io::Result<()>`. The caller pushes pre-sorted values for one entry.
/// An entry with no values is written as a hole (header == 0).
///
/// Header positions are accumulated via a zstd streaming encoder rather than
/// a plain `Vec<i32>` to avoid a ~2 GB peak allocation on large dumps. See
/// `docs/mat-cache.md` for the full RSS strategy.
pub fn write_sorted_cb<W, F>(w: W, n_entries: usize, mut f: F) -> io::Result<W>
where
    W: Write,
    F: FnMut(&mut dyn FnMut(i32) -> io::Result<()>) -> io::Result<()>,
{
    let mut cw = CountingWriter::new(w);
    let mut body = IntIndexStreamer::new(&mut cw);
    let mut body_size: i64 = 0;

    // Compress header values via a zstd streaming encoder. Rather than storing
    // raw body positions (which span 0..1.65B and compress poorly as LE i32s),
    // we delta-encode: for filled entries we store the delta from the previous
    // filled entry's position (= the number of values pushed for that entry,
    // typically 3-10); for holes we store 0 with a negative marker (-1 sentinel).
    // Deltas are small integers → zstd achieves ~10x+ compression → blob ~100 MB
    // instead of ~1.2 GB for raw positions.
    //
    // Wire format per entry: i32 LE
    //   0        = hole (header[i] == 0)
    //   delta+1  = filled entry, delta = header_val - prev_header_val (delta ≥ 1)
    let hdr_out: Vec<u8> = Vec::new();
    let mut hdr_enc = zstd::stream::write::Encoder::new(hdr_out, 3).map_err(io::Error::other)?;
    let mut total_entries: usize = 0;
    let mut prev_pos: i64 = 0; // last non-zero header position emitted

    for _ in 0..n_entries {
        let mut had_values = false;
        let pos = body_size + 1;
        if pos >= (1i64 << 32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "header2/PosIndexStreamer path not implemented; bodyPos {pos} at entry {total_entries}"
                ),
            ));
        }
        {
            let body_ref = &mut body;
            let body_size_ref = &mut body_size;
            let had_ref = &mut had_values;
            f(&mut |v: i32| {
                if !*had_ref {
                    *had_ref = true;
                }
                body_ref.push(v)?;
                *body_size_ref += 1;
                Ok(())
            })?;
        }
        let encoded: i32 = if had_values {
            let delta = pos - prev_pos; // ≥ 1
            prev_pos = pos;
            (delta + 1) as i32 // stored as delta+1 so 0 is unambiguously a hole
        } else {
            0
        };
        hdr_enc
            .write_all(&encoded.to_le_bytes())
            .map_err(io::Error::other)?;
        total_entries += 1;
    }
    let hdr_blob = hdr_enc.finish().map_err(io::Error::other)?;

    body.finish()?;
    let divider = cw.bytes_written() as i64;
    let mut w = cw.into_inner();
    let mut hdr = IntIndexStreamer::with_position(&mut w, divider);

    // Decompress and reconstruct original header positions from deltas.
    let mut decoder = zstd::stream::Decoder::new(BufReader::new(&hdr_blob[..]))?;
    let mut buf = [0u8; 64 * 1024];
    let mut carry = [0u8; 4];
    let mut carry_len = 0usize;
    let mut written = 0usize;
    let mut running_pos: i64 = 0;
    loop {
        let n = decoder.read(&mut buf).map_err(io::Error::other)?;
        if n == 0 {
            break;
        }
        let mut i = 0usize;
        // Complete any partial i32 from the previous read.
        while carry_len > 0 && i < n {
            carry[carry_len] = buf[i];
            carry_len += 1;
            i += 1;
            if carry_len == 4 {
                let encoded = i32::from_le_bytes(carry);
                let hval = if encoded == 0 {
                    0
                } else {
                    running_pos += (encoded - 1) as i64;
                    running_pos as i32
                };
                hdr.push(hval)?;
                written += 1;
                carry_len = 0;
            }
        }
        while i + 4 <= n {
            let encoded = i32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
            let hval = if encoded == 0 {
                0
            } else {
                running_pos += (encoded - 1) as i64;
                running_pos as i32
            };
            hdr.push(hval)?;
            written += 1;
            i += 4;
        }
        while i < n {
            carry[carry_len] = buf[i];
            carry_len += 1;
            i += 1;
        }
    }
    debug_assert_eq!(carry_len, 0);
    debug_assert_eq!(written, total_entries);
    drop(hdr_blob);

    hdr.finish()?;
    w.write_all(&divider.to_be_bytes())?;
    Ok(w)
}

/// Low-memory streaming variant of [`write_unsorted`].
pub fn write_unsorted_iter_streaming<W, I, S>(w: W, entries: I) -> io::Result<W>
where
    W: Write,
    I: Iterator<Item = S>,
    S: AsRef<[i32]>,
{
    let mut cw = CountingWriter::new(w);
    let mut header: Vec<i32> = Vec::new();
    let mut body = IntIndexStreamer::new(&mut cw);
    let mut body_size: i64 = 0;
    for entry in entries {
        let values = entry.as_ref();
        let pos = body_size;
        if pos >= (1i64 << 32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "header2 path not implemented; pos {pos} at entry {}",
                    header.len()
                ),
            ));
        }
        header.push(pos as i32);
        body.push(values.len() as i32)?;
        body_size += 1;
        for &v in values.as_ref() {
            body.push(v)?;
            body_size += 1;
        }
    }
    body.finish()?;
    let divider = cw.bytes_written() as i64;
    let mut w = cw.into_inner();

    let mut hdr = IntIndexStreamer::with_position(&mut w, divider);
    for &pos in &header {
        hdr.push(pos)?;
    }
    hdr.finish()?;
    w.write_all(&divider.to_be_bytes())?;
    Ok(w)
}

/// Shared final flush: write body bytes, the header index (opened at `divider`),
/// then the trailing divider i64. Only the plain-int header path is supported;
/// see the module docs re: header2 / PosIndexStreamer.
fn write_1n_tail<W: Write>(
    w: &mut W,
    body_bytes: &[u8],
    header: &[i64],
    divider: i64,
) -> io::Result<()> {
    // Reject positions that would need MAT's header2 / 2^40 path.
    for (i, &pos) in header.iter().enumerate() {
        if pos >= (1i64 << 32) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "header2/PosIndexStreamer path not implemented; \
                     bodyPos {pos} exceeds 2^32 for index {i}"
                ),
            ));
        }
    }

    // 2. body bytes.
    w.write_all(body_bytes)?;

    // 3. header index, opened at file offset `divider` (pageStart[0] == divider).
    let mut hdr = IntIndexStreamer::with_position(&mut *w, divider);
    for &pos in header {
        hdr.push(pos as i32)?;
    }
    hdr.finish()?; // flushes header pages + header footer into `w`

    // 4. trailing divider.
    w.write_all(&divider.to_be_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mat::codec::{compress_int, decode_int};
    use crate::mat::int_index::PAGE_SIZE_INT;

    /// Parse an IntIndexStreamer footer located at the end of `region`.
    /// Returns (pages, page_size, size, page_starts) with page_starts as
    /// **absolute** file offsets (as stored).
    fn parse_footer(region: &[u8]) -> (usize, i32, i64, Vec<i64>) {
        let n = region.len();
        let size = i32::from_be_bytes(region[n - 4..n].try_into().unwrap());
        let page_size = i32::from_be_bytes(region[n - 8..n - 4].try_into().unwrap());
        let value_count = size as i64;
        let pages = (value_count as usize).div_ceil(page_size as usize);
        let entries = pages + 1;
        let footer_start = n - 8 - entries * 8;
        let mut page_starts = Vec::with_capacity(entries);
        for i in 0..entries {
            let off = footer_start + i * 8;
            page_starts.push(i64::from_be_bytes(region[off..off + 8].try_into().unwrap()));
        }
        (pages, page_size, value_count, page_starts)
    }

    /// Decode every page of an int body/header region into a flat Vec<i32>.
    /// `page_starts` are absolute file offsets; `base` is the offset of the
    /// region within `file` (0 for body, `divider` for header).
    fn decode_int_region(
        file: &[u8],
        base: i64,
        pages: usize,
        page_size: i32,
        size: i64,
        page_starts: &[i64],
    ) -> Vec<i32> {
        let mut values: Vec<i32> = Vec::with_capacity(size as usize);
        for i in 0..pages {
            let start = (page_starts[i]) as usize;
            let end = (page_starts[i + 1]) as usize;
            let _ = base;
            let n = std::cmp::min(page_size as usize, size as usize - i * page_size as usize);
            values.extend_from_slice(&decode_int(&file[start..end], n));
        }
        values
    }

    /// Reconstruct one sorted entry using MAT's `IntArray1NSortedReader.get`
    /// semantics: a `0` header is an empty entry; otherwise the end position is
    /// the next header that is `>= p0` (scanning past `0`/smaller holes), or
    /// `body.size + 1` for the last entry.
    fn sorted_get(header: &[i32], body_vals: &[i32], index: usize) -> Vec<i32> {
        let body_size_plus1 = (body_vals.len() + 1) as i64;
        let (p0, mut p1);
        if index + 1 < header.len() {
            p0 = header[index] as i64;
            if p0 == 0 {
                return Vec::new();
            }
            p1 = header[index + 1] as i64;
            let mut j = index + 2;
            while p1 < p0 && j < header.len() {
                p1 = header[j] as i64;
                j += 1;
            }
            if p1 < p0 {
                p1 = body_size_plus1;
            }
        } else {
            p0 = header[index] as i64;
            if p0 == 0 {
                return Vec::new();
            }
            p1 = body_size_plus1;
        }
        let start = (p0 - 1) as usize;
        let len = (p1 - p0) as usize;
        body_vals[start..start + len].to_vec()
    }

    fn first_diff(ours: &[u8], real: &[u8], divider: i64, header_end: usize) -> String {
        let min = std::cmp::min(ours.len(), real.len());
        let mut off = usize::MAX;
        for i in 0..min {
            if ours[i] != real[i] {
                off = i;
                break;
            }
        }
        if off == usize::MAX {
            return format!(
                "lengths differ: ours.len={} real.len={}",
                ours.len(),
                real.len()
            );
        }
        let region = if (off as i64) < divider {
            "body"
        } else if off < header_end {
            "header"
        } else {
            "trailing-divider"
        };
        format!(
            "first diff at offset {off} in {region} (divider={divider}, header_end={header_end}); \
             ours.len={} real.len={}",
            ours.len(),
            real.len()
        )
    }

    #[test]
    fn unsorted_roundtrip_small() {
        let entries: Vec<Vec<i32>> = vec![vec![7, 8, 9], vec![], vec![42], vec![1, 2, 3, 4, 5]];
        let file = write_unsorted(Vec::new(), &entries).unwrap();

        // Trailing divider.
        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());

        // Body region [0..divider).
        let body_region = &file[0..divider as usize];
        let (bpages, bpsize, bsize, bstarts) = parse_footer(body_region);
        assert_eq!(bstarts[0], 0, "body starts at 0");
        let body_vals = decode_int_region(&file, 0, bpages, bpsize, bsize, &bstarts);

        // Header region [divider..n-8).
        let hdr_region = &file[divider as usize..n - 8];
        let (hpages, hpsize, hsize, hstarts) = parse_footer(hdr_region);
        assert_eq!(hstarts[0], divider, "header starts at divider");
        assert_eq!(hsize as usize, entries.len(), "one header entry per object");
        // Header page_starts are absolute; shift the region-local decode.
        let hstarts_local: Vec<i64> = hstarts.iter().map(|&s| s - divider).collect();
        let hdr_vals = decode_int_region(hdr_region, 0, hpages, hpsize, hsize, &hstarts_local);

        // Reconstruct entries from header positions + length-prefix scheme.
        let mut recon: Vec<Vec<i32>> = Vec::new();
        for &pos in &hdr_vals {
            let p = pos as usize;
            let len = body_vals[p] as usize;
            recon.push(body_vals[p + 1..p + 1 + len].to_vec());
        }
        assert_eq!(recon, entries, "unsorted reconstruct");

        // Re-emit and confirm byte-identical.
        let again = write_unsorted(Vec::new(), &recon).unwrap();
        assert_eq!(again, file, "unsorted deterministic re-emit");
    }

    #[test]
    fn sorted_roundtrip_small() {
        // Include empty entries to exercise MAT's "unset header == 0" hole path.
        let entries: Vec<Vec<i32>> = vec![
            vec![100, 101],
            vec![],
            vec![200, 201, 202],
            vec![],
            vec![300],
            vec![],
        ];
        let file = write_sorted(Vec::new(), &entries).unwrap();

        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());

        let body_region = &file[0..divider as usize];
        let (bpages, bpsize, bsize, bstarts) = parse_footer(body_region);
        let body_vals = decode_int_region(&file, 0, bpages, bpsize, bsize, &bstarts);

        let hdr_region = &file[divider as usize..n - 8];
        let (hpages, hpsize, hsize, hstarts) = parse_footer(hdr_region);
        assert_eq!(hstarts[0], divider);
        assert_eq!(hsize as usize, entries.len());
        let hstarts_local: Vec<i64> = hstarts.iter().map(|&s| s - divider).collect();
        let hdr_vals = decode_int_region(hdr_region, 0, hpages, hpsize, hsize, &hstarts_local);

        // Reconstruct with MAT reader semantics (handles 0 holes).
        let recon: Vec<Vec<i32>> = (0..hdr_vals.len())
            .map(|i| sorted_get(&hdr_vals, &body_vals, i))
            .collect();
        assert_eq!(recon, entries, "sorted reconstruct");

        let again = write_sorted(Vec::new(), &recon).unwrap();
        assert_eq!(again, file, "sorted deterministic re-emit");
    }

    #[test]
    fn footer_len_helper() {
        // 3 values => 1 page => 2 pageStart entries.
        assert_eq!(int_footer_len(3), 2 * 8 + 8);
        // exactly PAGE_SIZE_INT => 1 page.
        assert_eq!(int_footer_len(PAGE_SIZE_INT), 2 * 8 + 8);
        // PAGE_SIZE_INT + 1 => 2 pages => 3 entries.
        assert_eq!(int_footer_len(PAGE_SIZE_INT + 1), 3 * 8 + 8);
    }

    #[test]
    fn sorted_all_empty_entries() {
        // All entries are empty — body should be zero bytes, every header = 0.
        let entries: Vec<Vec<i32>> = vec![vec![], vec![], vec![]];
        let file = write_sorted(Vec::new(), &entries).unwrap();

        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());

        // Body has 0 values.
        let body_region = &file[0..divider as usize];
        let (_, _, bsize, _) = parse_footer(body_region);
        assert_eq!(bsize, 0, "all-empty sorted body must have size 0");

        // Header has 3 entries, all zero.
        let hdr_region = &file[divider as usize..n - 8];
        let (hpages, hpsize, hsize, hstarts) = parse_footer(hdr_region);
        assert_eq!(hsize as usize, 3);
        let hstarts_local: Vec<i64> = hstarts.iter().map(|&s| s - divider).collect();
        let hdr_vals = decode_int_region(hdr_region, 0, hpages, hpsize, hsize, &hstarts_local);
        assert!(
            hdr_vals.iter().all(|&v| v == 0),
            "all-empty headers must be 0: {hdr_vals:?}"
        );

        // Re-emit must be deterministic.
        let again = write_sorted(Vec::new(), &entries).unwrap();
        assert_eq!(again, file);
    }

    #[test]
    fn unsorted_all_empty_entries() {
        // Empty entries in unsorted layout: each emits len=0, then body_vals[pos]=0.
        let entries: Vec<Vec<i32>> = vec![vec![], vec![], vec![]];
        let file = write_unsorted(Vec::new(), &entries).unwrap();

        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());

        let body_region = &file[0..divider as usize];
        let (bpages, bpsize, bsize, bstarts) = parse_footer(body_region);
        // 3 length ints emitted (each = 0).
        assert_eq!(bsize, 3, "unsorted all-empty body has 3 length-0 ints");
        let body_vals = decode_int_region(&file, 0, bpages, bpsize, bsize, &bstarts);
        assert!(
            body_vals.iter().all(|&v| v == 0),
            "all length ints must be 0"
        );

        // Reconstruct.
        let hdr_region = &file[divider as usize..n - 8];
        let (hpages, hpsize, hsize, hstarts) = parse_footer(hdr_region);
        let hstarts_local: Vec<i64> = hstarts.iter().map(|&s| s - divider).collect();
        let hdr_vals = decode_int_region(hdr_region, 0, hpages, hpsize, hsize, &hstarts_local);
        let mut recon: Vec<Vec<i32>> = Vec::new();
        for &pos in &hdr_vals {
            let p = pos as usize;
            let len = body_vals[p] as usize;
            recon.push(body_vals[p + 1..p + 1 + len].to_vec());
        }
        assert_eq!(recon, entries);
    }

    #[test]
    fn sorted_single_entry_per_object() {
        // Each object has exactly 1 value.
        let entries: Vec<Vec<i32>> = vec![vec![10], vec![20], vec![30]];
        let file = write_sorted(Vec::new(), &entries).unwrap();
        let (_, _, recon) = decode_sorted_file(&file);
        assert_eq!(recon, entries);
        let again = write_sorted(Vec::new(), &recon).unwrap();
        assert_eq!(again, file);
    }

    #[test]
    fn unsorted_single_entry_per_object() {
        let entries: Vec<Vec<i32>> = vec![vec![10], vec![20], vec![30]];
        let file = write_unsorted(Vec::new(), &entries).unwrap();
        let (_, _, recon) = decode_unsorted_file(&file);
        assert_eq!(recon, entries);
        let again = write_unsorted(Vec::new(), &recon).unwrap();
        assert_eq!(again, file);
    }

    #[test]
    fn sorted_mixed_empty_and_nonempty() {
        // Interleaved empty/non-empty — exercises hole detection in header.
        let entries: Vec<Vec<i32>> = vec![vec![], vec![1, 2, 3], vec![], vec![], vec![99], vec![]];
        let file = write_sorted(Vec::new(), &entries).unwrap();
        let (_, _, recon) = decode_sorted_file(&file);
        assert_eq!(recon, entries, "sorted mixed roundtrip");
        let again = write_sorted(Vec::new(), &recon).unwrap();
        assert_eq!(again, file, "sorted mixed deterministic");
    }

    #[test]
    fn unsorted_mixed_empty_and_nonempty() {
        let entries: Vec<Vec<i32>> = vec![vec![5, 6], vec![], vec![7], vec![], vec![8, 9, 10]];
        let file = write_unsorted(Vec::new(), &entries).unwrap();
        let (_, _, recon) = decode_unsorted_file(&file);
        assert_eq!(recon, entries, "unsorted mixed roundtrip");
        let again = write_unsorted(Vec::new(), &recon).unwrap();
        assert_eq!(again, file, "unsorted mixed deterministic");
    }

    #[test]
    fn sorted_single_object_single_value() {
        let entries: Vec<Vec<i32>> = vec![vec![42]];
        let file = write_sorted(Vec::new(), &entries).unwrap();
        let (_, _, recon) = decode_sorted_file(&file);
        assert_eq!(recon, entries);
    }

    #[test]
    fn sorted_empty_file() {
        // zero objects
        let entries: Vec<Vec<i32>> = vec![];
        let file = write_sorted(Vec::new(), &entries).unwrap();
        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());
        // header has 0 entries, body has 0 entries.
        let body_region = &file[0..divider as usize];
        let (_, _, bsize, _) = parse_footer(body_region);
        assert_eq!(bsize, 0);
        let hdr_region = &file[divider as usize..n - 8];
        let (_, _, hsize, _) = parse_footer(hdr_region);
        assert_eq!(hsize, 0);
    }

    #[test]
    fn unsorted_empty_file() {
        let entries: Vec<Vec<i32>> = vec![];
        let file = write_unsorted(Vec::new(), &entries).unwrap();
        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());
        let body_region = &file[0..divider as usize];
        let (_, _, bsize, _) = parse_footer(body_region);
        assert_eq!(bsize, 0);
        let hdr_region = &file[divider as usize..n - 8];
        let (_, _, hsize, _) = parse_footer(hdr_region);
        assert_eq!(hsize, 0);
    }

    #[test]
    fn rejects_oversized_position() {
        // A synthetic header position >= 2^32 must be rejected. We can't easily
        // build a 4 GiB body, so drive write_1n_tail directly.
        let body_bytes = compress_int(&[0]);
        let header = vec![1i64 << 33];
        let mut out = Vec::new();
        let err = write_1n_tail(&mut out, &body_bytes, &header, body_bytes.len() as i64)
            .expect_err("must reject 2^32+ position");
        assert!(err.to_string().contains("exceeds 2^32"), "err: {err}");
    }

    /// Decode a real MAT unsorted IntArray1N file into `entries`.
    fn decode_unsorted_file(file: &[u8]) -> (i64, usize, Vec<Vec<i32>>) {
        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());
        let body_region = &file[0..divider as usize];
        let (bpages, bpsize, bsize, bstarts) = parse_footer(body_region);
        let body_vals = decode_int_region(file, 0, bpages, bpsize, bsize, &bstarts);

        let hdr_region = &file[divider as usize..n - 8];
        let (hpages, hpsize, hsize, hstarts) = parse_footer(hdr_region);
        let hstarts_local: Vec<i64> = hstarts.iter().map(|&s| s - divider).collect();
        let hdr_vals = decode_int_region(hdr_region, 0, hpages, hpsize, hsize, &hstarts_local);

        let mut entries: Vec<Vec<i32>> = Vec::with_capacity(hdr_vals.len());
        for &pos in &hdr_vals {
            let p = pos as usize;
            let len = body_vals[p] as usize;
            entries.push(body_vals[p + 1..p + 1 + len].to_vec());
        }
        (divider, n - 8, entries)
    }

    /// Decode a real MAT sorted IntArray1N file into `entries`.
    fn decode_sorted_file(file: &[u8]) -> (i64, usize, Vec<Vec<i32>>) {
        let n = file.len();
        let divider = i64::from_be_bytes(file[n - 8..n].try_into().unwrap());
        let body_region = &file[0..divider as usize];
        let (bpages, bpsize, bsize, bstarts) = parse_footer(body_region);
        let body_vals = decode_int_region(file, 0, bpages, bpsize, bsize, &bstarts);

        let hdr_region = &file[divider as usize..n - 8];
        let (hpages, hpsize, hsize, hstarts) = parse_footer(hdr_region);
        let hstarts_local: Vec<i64> = hstarts.iter().map(|&s| s - divider).collect();
        let hdr_vals = decode_int_region(hdr_region, 0, hpages, hpsize, hsize, &hstarts_local);

        let entries: Vec<Vec<i32>> = (0..hdr_vals.len())
            .map(|i| sorted_get(&hdr_vals, &body_vals, i))
            .collect();
        (divider, n - 8, entries)
    }

    #[test]
    fn matches_real_domout() {
        let path = "/tmp/matidx/dump_.domOut.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_domout: fixture absent at {path}");
            return;
        };
        let (divider, header_end, entries) = decode_unsorted_file(&real);
        let ours = write_unsorted(Vec::new(), &entries).unwrap();
        if ours != real {
            panic!(
                "matches_real_domout byte mismatch: {}",
                first_diff(&ours, &real, divider, header_end)
            );
        }
    }

    #[test]
    fn matches_real_outbound() {
        let path = "/tmp/matidx/dump_.outbound.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_outbound: fixture absent at {path}");
            return;
        };
        let (divider, header_end, entries) = decode_sorted_file(&real);
        let ours = write_sorted(Vec::new(), &entries).unwrap();
        if ours != real {
            panic!(
                "matches_real_outbound byte mismatch: {}",
                first_diff(&ours, &real, divider, header_end)
            );
        }
    }

    #[test]
    fn matches_real_inbound() {
        let path = "/tmp/matidx/dump_.inbound.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_inbound: fixture absent at {path}");
            return;
        };
        let (divider, header_end, entries) = decode_sorted_file(&real);
        let ours = write_sorted(Vec::new(), &entries).unwrap();
        if ours != real {
            panic!(
                "matches_real_inbound byte mismatch: {}",
                first_diff(&ours, &real, divider, header_end)
            );
        }
    }
}
