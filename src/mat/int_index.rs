//! Streaming writers for MAT IntIndex / LongIndex files.
//!
//! Mirrors `IndexWriter.IntIndexStreamer` / `LongIndexStreamer` from Eclipse MAT
//! 1.13.0. Each page is compressed and written as soon as it fills, so the full
//! value array is never buffered in memory.
//!
//! File layout (all big-endian):
//!   body   = concat of compressed pages (each `compress_int`/`compress_long`)
//!   footer = pageStart[0..=pages] (i64 each) ++ pageSize:i32 ++ size:i32
//! where pageStart[0]=0, pageStart[i]=start byte offset of page i, and the
//! final entry pageStart[pages] = total body length (= start of footer).

use std::io::{self, Write};

use super::codec::{compress_int, compress_long};

pub const PAGE_SIZE_INT: usize = 1_000_000;
pub const PAGE_SIZE_LONG: usize = 500_000;

/// Streaming writer for a MAT IntIndex (one2one int index).
#[allow(dead_code)]
pub struct IntIndexStreamer<W: Write> {
    w: W,
    page: Vec<i32>,
    /// Cumulative byte offsets: starts as `[0]`, each flush appends
    /// `last + compressed_page_len`. After all pages, the last element equals
    /// the body length (= start of the pageStart array in the footer).
    page_starts: Vec<i64>,
    /// Absolute byte offset of the next compressed page. Initialised to the
    /// stream's start position (0 for a normal body, `divider` for a header
    /// index) and advanced by each flushed page's compressed length, so that
    /// every appended `page_starts` entry is already an absolute file offset.
    written: i64,
    /// Total number of values pushed.
    size: i64,
}

#[allow(dead_code)]
impl<W: Write> IntIndexStreamer<W> {
    pub fn new(w: W) -> Self {
        Self::with_position(w, 0)
    }

    /// Like `new`, but starts the `pageStart` array at `start` instead of 0.
    ///
    /// MAT's `IndexWriter.openStream(out, position)` lets a streamer append its
    /// pages at a non-zero file offset (used for the header index of an
    /// IntArray1N file, which begins right after the body region). `written`
    /// is seeded with `start`; each recorded pageStart is therefore an absolute
    /// file offset, and the final footer entry is `start + body_len` (= start
    /// of that index's own footer within the file).
    pub fn with_position(w: W, start: i64) -> Self {
        Self {
            w,
            page: Vec::with_capacity(PAGE_SIZE_INT),
            page_starts: vec![start],
            written: start,
            size: 0,
        }
    }

    /// Buffer one value; flush the current page if it becomes full.
    pub fn push(&mut self, v: i32) -> io::Result<()> {
        self.page.push(v);
        self.size += 1;
        if self.page.len() == PAGE_SIZE_INT {
            self.flush_page()?;
        }
        Ok(())
    }

    fn flush_page(&mut self) -> io::Result<()> {
        let bytes = compress_int(&self.page);
        self.w.write_all(&bytes)?;
        self.written += bytes.len() as i64;
        // Record the start offset of the *next* page (= end of this one).
        self.page_starts.push(self.written);
        self.page.clear();
        Ok(())
    }

    /// Flush the final partial page (if any) and write the footer.
    pub fn finish(mut self) -> io::Result<W> {
        // MAT flushes the trailing partial page only if it is non-empty. If the
        // total is an exact multiple of PAGE_SIZE the last full page was already
        // flushed by `push`, leaving `self.page` empty here.
        if !self.page.is_empty() {
            self.flush_page()?;
        }
        write_footer(
            &mut self.w,
            &self.page_starts,
            PAGE_SIZE_INT as i32,
            self.size,
            true,
        )?;
        Ok(self.w)
    }
}

/// Streaming writer for a MAT LongIndex (one2one long index).
#[allow(dead_code)]
pub struct LongIndexStreamer<W: Write> {
    w: W,
    page: Vec<i64>,
    page_starts: Vec<i64>,
    written: i64,
    size: i64,
}

#[allow(dead_code)]
impl<W: Write> LongIndexStreamer<W> {
    pub fn new(w: W) -> Self {
        Self {
            w,
            page: Vec::with_capacity(PAGE_SIZE_LONG),
            page_starts: vec![0],
            written: 0,
            size: 0,
        }
    }

    pub fn push(&mut self, v: i64) -> io::Result<()> {
        self.page.push(v);
        self.size += 1;
        if self.page.len() == PAGE_SIZE_LONG {
            self.flush_page()?;
        }
        Ok(())
    }

    fn flush_page(&mut self) -> io::Result<()> {
        let bytes = compress_long(&self.page);
        self.w.write_all(&bytes)?;
        self.written += bytes.len() as i64;
        self.page_starts.push(self.written);
        self.page.clear();
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        if !self.page.is_empty() {
            self.flush_page()?;
        }
        // LongIndexStreamer in MAT writes `size` with a plain (int) cast; it does
        // not use the huge-form negative encoding that IntIndexStreamer uses.
        write_footer(
            &mut self.w,
            &self.page_starts,
            PAGE_SIZE_LONG as i32,
            self.size,
            false,
        )?;
        Ok(self.w)
    }
}

/// Write the shared footer: pageStart[] (i64 BE) then pageSize (i32 BE) then
/// size (i32 BE). When `huge_form` is set and `size > i32::MAX`, `size` is
/// written as the MAT negative encoding `-(((size + pageSize - 1) % pageSize) + 1)`.
fn write_footer<W: Write>(
    w: &mut W,
    page_starts: &[i64],
    page_size: i32,
    size: i64,
    huge_form: bool,
) -> io::Result<()> {
    for &ps in page_starts {
        w.write_all(&ps.to_be_bytes())?;
    }
    w.write_all(&page_size.to_be_bytes())?;
    let s: i32 = if huge_form && size > i32::MAX as i64 {
        let ps = page_size as i64;
        -(((size + ps - 1) % ps) + 1) as i32
    } else {
        size as i32
    };
    w.write_all(&s.to_be_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mat::codec::decode_long;

    /// Parse the footer of a MAT index file: returns (pages, page_size, size, page_starts).
    fn parse_footer(buf: &[u8]) -> (usize, i32, i64, Vec<i64>) {
        let n = buf.len();
        let size = i32::from_be_bytes(buf[n - 4..n].try_into().unwrap());
        let page_size = i32::from_be_bytes(buf[n - 8..n - 4].try_into().unwrap());
        // Recover value count. For our fixtures size is positive.
        let value_count = size as i64;
        let pages = (value_count as usize).div_ceil(page_size as usize);
        let entries = pages + 1;
        let footer_start = n - 8 - entries * 8;
        let mut page_starts = Vec::with_capacity(entries);
        for i in 0..entries {
            let off = footer_start + i * 8;
            page_starts.push(i64::from_be_bytes(buf[off..off + 8].try_into().unwrap()));
        }
        (pages, page_size, value_count, page_starts)
    }

    #[test]
    fn int_three_values() {
        let mut s = IntIndexStreamer::new(Vec::new());
        for v in [10i32, 20, 30] {
            s.push(v).unwrap();
        }
        let buf = s.finish().unwrap();
        let (pages, page_size, size, page_starts) = parse_footer(&buf);
        assert_eq!(size, 3);
        assert_eq!(page_size, PAGE_SIZE_INT as i32);
        assert_eq!(pages, 1);
        assert_eq!(page_starts.len(), 2);
        assert_eq!(page_starts[0], 0);
        // Last pageStart entry == body length == start of the footer.
        let body_len = (buf.len() - 8 - page_starts.len() * 8) as i64;
        assert_eq!(*page_starts.last().unwrap(), body_len);
    }

    #[test]
    fn long_three_values() {
        let mut s = LongIndexStreamer::new(Vec::new());
        for v in [0x1000i64, 0x2000, 0x1_0000_0000] {
            s.push(v).unwrap();
        }
        let buf = s.finish().unwrap();
        let (pages, page_size, size, page_starts) = parse_footer(&buf);
        assert_eq!(size, 3);
        assert_eq!(page_size, PAGE_SIZE_LONG as i32);
        assert_eq!(pages, 1);
        assert_eq!(page_starts.len(), 2);
        assert_eq!(page_starts[0], 0);
        let body_len = (buf.len() - 8 - page_starts.len() * 8) as i64;
        assert_eq!(*page_starts.last().unwrap(), body_len);
    }

    /// The definition of done: byte-for-byte reproduction of a real MAT LongIndex.
    #[test]
    fn matches_real_idx() {
        let path = "/tmp/matidx/dump_.idx.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_idx: fixture absent at {path}");
            return;
        };
        let (pages, page_size, size, page_starts) = parse_footer(&real);
        assert_eq!(page_size, PAGE_SIZE_LONG as i32, "fixture pageSize");

        // Decode every page back into i64 values.
        let mut values: Vec<i64> = Vec::with_capacity(size as usize);
        for i in 0..pages {
            let start = page_starts[i] as usize;
            let end = page_starts[i + 1] as usize;
            let n = std::cmp::min(PAGE_SIZE_LONG, size as usize - i * PAGE_SIZE_LONG);
            let decoded = decode_long(&real[start..end], n);
            values.extend_from_slice(&decoded);
        }
        assert_eq!(values.len() as i64, size, "decoded value count");

        // Re-emit through our streamer.
        let mut s = LongIndexStreamer::new(Vec::new());
        for &v in &values {
            s.push(v).unwrap();
        }
        let ours = s.finish().unwrap();

        if ours != real {
            let (mut off, min) = (usize::MAX, std::cmp::min(ours.len(), real.len()));
            for i in 0..min {
                if ours[i] != real[i] {
                    off = i;
                    break;
                }
            }
            let body_len = *page_starts.last().unwrap() as usize;
            let region = if off == usize::MAX {
                "length differs".to_string()
            } else if off < body_len {
                format!("body (page-relative area, body_len={body_len})")
            } else {
                "footer".to_string()
            };
            panic!(
                "byte mismatch: ours.len={} real.len={} first diff at offset {} in {}",
                ours.len(),
                real.len(),
                off,
                region
            );
        }
        assert_eq!(ours, real);
    }
}
