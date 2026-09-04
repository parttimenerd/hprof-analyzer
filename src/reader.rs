//! Streaming byte reader that fronts the HPROF dump file. Transparently
//! decompresses gzip'd dumps (magic sniff) and serves the big-endian
//! `u1`/`u2`/`u4`/`u8`/`id` primitives the parser consumes, buffering in large
//! chunks so a multi-gigabyte scan stays sequential and allocation-light.

use flate2::read::MultiGzDecoder;
use std::{
    fs::File,
    io::{self, BufReader, Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// A sentinel `io::Error` kind used to signal `HEAP_DUMP_END` through the
/// record-body closure in the pass1/pass2 scan loops without breaking the
/// outer loop via a `break` that cannot cross a closure boundary.
///
/// Callers match on `e.kind() == HEAP_DUMP_END_KIND` and `break` the outer loop.
pub const HEAP_DUMP_END_KIND: io::ErrorKind = io::ErrorKind::ConnectionAborted;

const BUF_CAP: usize = 8 << 20; // 8 MiB refill chunk

/// Streaming HPROF reader with a large internal buffer.
///
/// All primitive reads (`u1`/`u2`/`u4`/`u8`/`id`) and `skip`/`read_bytes_reuse`
/// pull from an in-memory buffer, refilling in 1 MiB chunks. This avoids the
/// per-primitive virtual-dispatch + bounds-checked `read_exact` overhead that
/// dominates multi-gigabyte scans.
pub struct HprofReader {
    pub format: String,
    pub id_size: u8,
    pub timestamp_ms: u64,
    inner: Box<dyn Read>,
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    /// Total bytes delivered to callers since `open()`. Used to record the
    /// HPROF file offset of each object record for the MAT `o2hprof` index.
    bytes_consumed: u64,
    /// Set to `true` if the underlying gzip stream was truncated/corrupt.
    /// Callers can check this after a scan to include a "partial data" notice.
    truncated_input: Arc<AtomicBool>,
}

/// Wraps `GzDecoder` and converts gzip checksum / trailer errors into clean
/// EOF. This lets truncated gzip streams be read up to the point of truncation
/// rather than failing with an error when the gzip footer is missing or corrupt.
/// A warning is printed to stderr on the first such error so the user knows the
/// report may be partial.
pub(crate) struct LenientGzDecoder<R: Read> {
    inner: MultiGzDecoder<R>,
    warned: bool,
    truncated: Arc<AtomicBool>,
}

impl<R: Read> LenientGzDecoder<R> {
    pub(crate) fn new(r: R, truncated: Arc<AtomicBool>) -> Self {
        Self {
            inner: MultiGzDecoder::new(r),
            warned: false,
            truncated,
        }
    }
}

impl<R: Read> Read for LenientGzDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buf) {
            Ok(n) => Ok(n),
            Err(e)
                if e.kind() == io::ErrorKind::UnexpectedEof
                    || e.kind() == io::ErrorKind::InvalidData =>
            {
                self.truncated.store(true, Ordering::Relaxed);
                if !self.warned {
                    self.warned = true;
                    eprintln!(
                        "warning: gzip stream ended prematurely ({}); \
                         the dump appears truncated — report will cover \
                         whatever data was successfully decompressed",
                        e
                    );
                }
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

impl HprofReader {
    /// Open a dump (gzip/zip/tar.gz auto-detected) and consume its HPROF header.
    pub fn open(path: &str) -> io::Result<Self> {
        let lower = path.to_ascii_lowercase();

        // tar.gz: gunzip then stream the first .hprof entry from the tar archive.
        #[cfg(feature = "native")]
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            return Self::open_tar_gz(path);
        }

        let file = File::open(path)?;
        let mut peek = BufReader::new(file);
        let mut magic = [0u8; 4];
        peek.read_exact(&mut magic)?;

        // ZIP archive (PK\x03\x04): re-open with Seek and extract the .hprof entry.
        #[cfg(feature = "native")]
        if magic[..2] == [0x50, 0x4b] {
            return Self::open_zip(path);
        }

        let truncated = Arc::new(AtomicBool::new(false));
        let stream = Cursor::new(magic.to_vec()).chain(peek);
        let inner: Box<dyn Read> = if magic[..2] == [0x1f, 0x8b] {
            Box::new(LenientGzDecoder::new(stream, Arc::clone(&truncated)))
        } else {
            Box::new(stream)
        };
        let mut r = HprofReader {
            format: String::new(),
            id_size: 4,
            timestamp_ms: 0,
            inner,
            buf: vec![0u8; BUF_CAP],
            pos: 0,
            end: 0,
            bytes_consumed: 0,
            truncated_input: truncated,
        };
        r.read_header()?;
        Ok(r)
    }

    /// Open a `.hprof.tar.gz` (or `.tgz`), find the first `.hprof` entry, and
    /// stream it through the parser. The tar archive is read sequentially — no
    /// Seek required — so multi-gigabyte archives decompress on-the-fly.
    ///
    /// Truncated archives are handled leniently: gzip checksum/trailer errors
    /// are treated as EOF so the HPROF record loop can process whatever data
    /// was successfully decompressed.
    #[cfg(feature = "native")]
    fn open_tar_gz(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let truncated = Arc::new(AtomicBool::new(false));
        let gz = LenientGzDecoder::new(BufReader::new(file), Arc::clone(&truncated));
        // Leak the archive to give it `'static` lifetime so `tar::Entry` (which
        // borrows it) can be boxed as `Box<dyn Read + 'static>`. The archive's
        // only resource is the file handle, which is consumed by reading the
        // entry stream; the small archive wrapper struct (<1 KB) is the only
        // memory that is permanently leaked per call.
        let archive: &'static mut tar::Archive<_> = Box::leak(Box::new(tar::Archive::new(gz)));
        let entries = match archive.entries() {
            Ok(e) => e,
            Err(_) if truncated.load(std::sync::atomic::Ordering::Relaxed) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "tar archive truncated before any entries",
                ));
            }
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) if truncated.load(std::sync::atomic::Ordering::Relaxed) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "tar archive truncated mid-entry",
                    ));
                }
                Err(e) => return Err(e),
            };
            let ends_with_hprof = entry.path_bytes().to_ascii_lowercase().ends_with(b".hprof");
            if ends_with_hprof {
                let inner: Box<dyn Read> = Box::new(entry);
                let mut r = HprofReader {
                    format: String::new(),
                    id_size: 4,
                    timestamp_ms: 0,
                    inner,
                    buf: vec![0u8; BUF_CAP],
                    pos: 0,
                    end: 0,
                    bytes_consumed: 0,
                    truncated_input: truncated,
                };
                r.read_header()?;
                return Ok(r);
            }
        }
        if truncated.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tar archive truncated before .hprof entry",
            ));
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no .hprof entry found in tar archive",
        ))
    }

    /// Open a `.hprof.zip`, find the first `.hprof` entry, and stream it through
    /// the parser. Re-opens the file to get a `Seek`-capable handle; the entry is
    /// decompressed on-the-fly with no full-file buffering.
    #[cfg(feature = "native")]
    fn open_zip(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let idx = (0..archive.len())
            .find(|&i| {
                archive
                    .by_index(i)
                    .map(|f| f.name().to_ascii_lowercase().ends_with(".hprof"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "no .hprof entry found in zip")
            })?;
        let entry = archive
            .by_index(idx)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // `ZipFile` is `Read` but borrows `archive`, so we must read into a Vec.
        // For typical .hprof.zip sizes (tens to hundreds of MB decompressed) this
        // is acceptable; the parser's own pass1 scan then proceeds from a Cursor.
        // Cap the pre-allocation at 2 GiB to avoid OOM on corrupt ZIP size fields.
        let cap = (entry.size() as usize).min(2 * 1024 * 1024 * 1024);
        let mut hprof_bytes = Vec::with_capacity(cap);
        let mut entry = entry;
        entry.read_to_end(&mut hprof_bytes)?;
        Self::from_reader(Cursor::new(hprof_bytes))
    }

    /// Construct a reader from any `Read` implementation.
    /// Used by `HprofSource::open` for the `Bytes` variant (including WASM).
    /// The caller may pass a pre-wired `truncated` flag; pass `None` to get a
    /// fresh one (always false for non-gzip inputs).
    pub(crate) fn from_reader_with_flag(
        r: impl Read + 'static,
        truncated: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let mut reader = HprofReader {
            format: String::new(),
            id_size: 4,
            timestamp_ms: 0,
            inner: Box::new(r),
            buf: vec![0u8; BUF_CAP],
            pos: 0,
            end: 0,
            bytes_consumed: 0,
            truncated_input: truncated,
        };
        reader.read_header()?;
        Ok(reader)
    }

    /// Construct a reader from any `Read` implementation with a fresh (always-false)
    /// truncation flag. Used by `open_zip` and tests.
    pub(crate) fn from_reader(r: impl Read + 'static) -> io::Result<Self> {
        Self::from_reader_with_flag(r, Arc::new(AtomicBool::new(false)))
    }

    /// Returns `true` if the underlying gzip stream was truncated or corrupt.
    /// When true, the report covers only the records that were decompressed before
    /// the stream ended prematurely.
    pub fn is_truncated(&self) -> bool {
        self.truncated_input.load(Ordering::Relaxed)
    }

    fn read_header(&mut self) -> io::Result<()> {
        let mut s = Vec::new();
        loop {
            let b = self.u1()?;
            if b == 0 {
                break;
            }
            s.push(b);
        }
        self.format = String::from_utf8_lossy(&s).into_owned();
        let id_size = self.u4()?;
        if id_size != 4 && id_size != 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported id_size in HPROF header: {id_size} (expected 4 or 8)"),
            ));
        }
        self.id_size = id_size as u8;
        self.timestamp_ms = self.u8()?;
        Ok(())
    }

    /// Refill the buffer, preserving any unconsumed bytes at the front.
    /// Returns the number of bytes now available (`end - pos`).
    #[cold]
    fn refill(&mut self) -> io::Result<usize> {
        // Move leftover bytes to the front.
        let leftover = self.end - self.pos;
        if leftover > 0 {
            self.buf.copy_within(self.pos..self.end, 0);
        }
        self.pos = 0;
        self.end = leftover;
        while self.end < self.buf.len() {
            let n = self.inner.read(&mut self.buf[self.end..])?;
            if n == 0 {
                break;
            }
            self.end += n;
        }
        Ok(self.end - self.pos)
    }

    /// Ensure at least `n` bytes are available in the buffer (n <= BUF_CAP).
    /// Returns Err(UnexpectedEof) if the stream ends first.
    #[inline]
    fn ensure(&mut self, n: usize) -> io::Result<()> {
        if self.end - self.pos >= n {
            return Ok(());
        }
        self.refill()?;
        if self.end - self.pos >= n {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unexpected eof",
            ))
        }
    }

    /// Read one unsigned byte.
    #[inline]
    pub fn u1(&mut self) -> io::Result<u8> {
        if self.pos >= self.end {
            self.ensure(1)?;
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        self.bytes_consumed += 1;
        Ok(b)
    }

    /// Read a big-endian `u16`.
    #[inline]
    pub fn u2(&mut self) -> io::Result<u16> {
        self.ensure(2)?;
        let p = self.pos;
        let v = u16::from_be_bytes([self.buf[p], self.buf[p + 1]]);
        self.pos = p + 2;
        self.bytes_consumed += 2;
        Ok(v)
    }

    /// Read a big-endian `u32`.
    #[inline]
    pub fn u4(&mut self) -> io::Result<u32> {
        self.ensure(4)?;
        let p = self.pos;
        let v = u32::from_be_bytes([
            self.buf[p],
            self.buf[p + 1],
            self.buf[p + 2],
            self.buf[p + 3],
        ]);
        self.pos = p + 4;
        self.bytes_consumed += 4;
        Ok(v)
    }

    /// Read a big-endian `u64`.
    #[inline]
    pub fn u8(&mut self) -> io::Result<u64> {
        self.ensure(8)?;
        let p = self.pos;
        let v = u64::from_be_bytes([
            self.buf[p],
            self.buf[p + 1],
            self.buf[p + 2],
            self.buf[p + 3],
            self.buf[p + 4],
            self.buf[p + 5],
            self.buf[p + 6],
            self.buf[p + 7],
        ]);
        self.pos = p + 8;
        self.bytes_consumed += 8;
        Ok(v)
    }

    /// Read an object id (`u4` or `u8` per the header's `id_size`).
    #[inline]
    pub fn id(&mut self) -> io::Result<u64> {
        match self.id_size {
            4 => Ok(self.u4()? as u64),
            8 => self.u8(),
            s => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported id_size: {s}"),
            )),
        }
    }

    /// Advance the stream by `n` bytes without materializing them.
    pub fn skip(&mut self, mut n: u64) -> io::Result<()> {
        let skipped_total = n;
        while n > 0 {
            let avail = self.end - self.pos;
            if avail == 0 {
                if self.refill()? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated dump: unexpected end of file while skipping record data",
                    ));
                }
                continue;
            }
            let take = (avail as u64).min(n) as usize;
            self.pos += take;
            n -= take as u64;
        }
        self.bytes_consumed += skipped_total;
        Ok(())
    }

    /// Read the next top-level HPROF record header (tag + timestamp + length).
    /// Returns `Ok(None)` on clean EOF (including truncated gzip streams).
    /// Any mid-header EOF is also treated as clean EOF rather than an error,
    /// since a truncated dump can cut off in the middle of a header.
    pub fn next_record(&mut self) -> io::Result<Option<(u8, u64)>> {
        let tag = match self.u1() {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            other => other?,
        };
        let _ts = match self.u4() {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            other => other?,
        };
        let length = match self.u4() {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            other => other?,
        };
        Ok(Some((tag, length as u64)))
    }

    /// Read exactly `n` bytes into a freshly allocated `Vec`.
    pub fn read_bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        if n > 1 << 30 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("read_bytes: requested {n} bytes exceeds 1 GiB sanity limit"),
            ));
        }
        let mut v = vec![0u8; n];
        self.read_into(&mut v)?;
        self.bytes_consumed += n as u64;
        Ok(v)
    }

    /// Like `read_bytes` but reuses an existing buffer to avoid repeated allocation.
    pub fn read_bytes_reuse(&mut self, buf: &mut Vec<u8>, n: usize) -> io::Result<()> {
        if n > 1 << 30 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("read_bytes_reuse: requested {n} bytes exceeds 1 GiB sanity limit"),
            ));
        }
        buf.resize(n, 0);
        self.read_into(buf)?;
        self.bytes_consumed += n as u64;
        Ok(())
    }

    /// Number of bytes delivered to callers since `open()`.
    /// Used to record the HPROF file offset of each object record.
    #[inline]
    pub fn bytes_consumed(&self) -> u64 {
        self.bytes_consumed
    }

    /// Fill `dst` completely from the internal buffer + underlying stream.
    pub(crate) fn read_into(&mut self, dst: &mut [u8]) -> io::Result<()> {
        let mut written = 0usize;
        // First, drain whatever is already buffered.
        while written < dst.len() {
            let avail = self.end - self.pos;
            if avail > 0 {
                let take = avail.min(dst.len() - written);
                dst[written..written + take].copy_from_slice(&self.buf[self.pos..self.pos + take]);
                self.pos += take;
                written += take;
            } else {
                // Buffer empty. For large remaining reads, read straight into dst
                // to bypass the intermediate buffer.
                let remaining = dst.len() - written;
                if remaining >= BUF_CAP {
                    self.inner.read_exact(&mut dst[written..]).map_err(|e| {
                        if e.kind() == io::ErrorKind::UnexpectedEof {
                            io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "truncated dump: unexpected end of file while reading record data",
                            )
                        } else {
                            e
                        }
                    })?;
                    written = dst.len();
                } else if self.refill()? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated dump: unexpected end of file while reading record data",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Optional machine-local dumps for smoke tests, supplied via env vars so no
    // absolute path is baked into the source. Unset => the test no-ops.
    fn dump_plain() -> Option<String> {
        std::env::var("HPROF_TEST_DUMP").ok()
    }
    fn dump_gz() -> Option<String> {
        std::env::var("HPROF_TEST_DUMP_GZ").ok()
    }

    #[test]
    fn read_header_plain() {
        let Some(dump) = dump_plain() else {
            return;
        };
        let r = HprofReader::open(&dump).unwrap();
        assert!(
            r.id_size == 4 || r.id_size == 8,
            "bad id_size {}",
            r.id_size
        );
        assert!(
            r.format.starts_with("JAVA PROFILE"),
            "bad format {:?}",
            r.format
        );
        assert!(r.timestamp_ms > 0, "timestamp should be nonzero");
    }

    #[test]
    fn read_header_gz() {
        let Some(dump) = dump_gz() else {
            return;
        };
        let r = HprofReader::open(&dump).unwrap();
        assert!(r.id_size == 4 || r.id_size == 8);
        assert!(r.format.starts_with("JAVA PROFILE"));
    }

    #[test]
    fn read_primitives() {
        let data: Vec<u8> = vec![
            0xAB, 0x12, 0x34, 0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
            0x08,
        ];
        let mut r = HprofReader {
            format: String::new(),
            id_size: 4,
            timestamp_ms: 0,
            inner: Box::new(io::Cursor::new(data)),
            buf: vec![0u8; BUF_CAP],
            pos: 0,
            end: 0,
            bytes_consumed: 0,
            truncated_input: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(r.u1().unwrap(), 0xAB);
        assert_eq!(r.u2().unwrap(), 0x1234);
        assert_eq!(r.u4().unwrap(), 0xDEADBEEF);
        assert_eq!(r.u8().unwrap(), 0x0102030405060708);
    }

    #[test]
    fn skip_and_read() {
        let data: Vec<u8> = (0..100u8).collect();
        let mut r = HprofReader {
            format: String::new(),
            id_size: 8,
            timestamp_ms: 0,
            inner: Box::new(io::Cursor::new(data)),
            buf: vec![0u8; BUF_CAP],
            pos: 0,
            end: 0,
            bytes_consumed: 0,
            truncated_input: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(r.u1().unwrap(), 0);
        r.skip(9).unwrap(); // skip 1..=9
        assert_eq!(r.u1().unwrap(), 10);
        let mut buf = Vec::new();
        r.read_bytes_reuse(&mut buf, 5).unwrap();
        assert_eq!(buf, vec![11, 12, 13, 14, 15]);
    }

    // Build a minimal HPROF header blob: NUL-terminated format string, a 4-byte
    // id_size, then an 8-byte timestamp.
    fn header_blob(id_size: u32) -> Vec<u8> {
        let mut v = b"JAVA PROFILE 1.0.2\0".to_vec();
        v.extend_from_slice(&id_size.to_be_bytes());
        v.extend_from_slice(&1u64.to_be_bytes());
        v
    }

    fn reader_over(data: Vec<u8>) -> HprofReader {
        HprofReader {
            format: String::new(),
            id_size: 0,
            timestamp_ms: 0,
            inner: Box::new(io::Cursor::new(data)),
            buf: vec![0u8; BUF_CAP],
            pos: 0,
            end: 0,
            bytes_consumed: 0,
            truncated_input: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn read_header_accepts_4_and_8() {
        for sz in [4u32, 8] {
            let mut r = reader_over(header_blob(sz));
            r.read_header().unwrap();
            assert_eq!(r.id_size, sz as u8);
            assert!(r.format.starts_with("JAVA PROFILE"));
        }
    }

    #[test]
    fn read_header_rejects_bad_id_size() {
        // 260 truncates to 4 as a u8 — must be rejected, not silently accepted.
        for sz in [0u32, 2, 16, 260] {
            let mut r = reader_over(header_blob(sz));
            let err = r.read_header().unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "sz={sz}");
        }
    }

    // Build a minimal valid HPROF dump: header + one minimal record.
    // Used to test truncated-gzip handling.
    fn minimal_hprof() -> Vec<u8> {
        let mut v = header_blob(4);
        // Append one STRING_IN_UTF8 (tag 0x01): timestamp(4) + length(4) + no bytes.
        v.push(0x01); // tag
        v.extend_from_slice(&0u32.to_be_bytes()); // timestamp
        v.extend_from_slice(&0u32.to_be_bytes()); // length = 0 (empty body)
        v
    }

    fn gzip_bytes(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn lenient_gz_decoder_clean_stream_not_truncated() {
        let gz = gzip_bytes(&minimal_hprof());
        let flag = Arc::new(AtomicBool::new(false));
        let mut dec = LenientGzDecoder::new(io::Cursor::new(gz), Arc::clone(&flag));
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        assert_eq!(out, minimal_hprof(), "clean stream decompresses correctly");
        assert!(
            !flag.load(Ordering::Relaxed),
            "clean stream should not set truncated flag"
        );
    }

    #[test]
    fn lenient_gz_decoder_truncated_stream_sets_flag() {
        let gz = gzip_bytes(&minimal_hprof());
        // Drop the last 4 bytes to truncate the gzip trailer.
        let truncated = gz[..gz.len() - 4].to_vec();
        let flag = Arc::new(AtomicBool::new(false));
        let mut dec = LenientGzDecoder::new(io::Cursor::new(truncated), Arc::clone(&flag));
        let mut out = Vec::new();
        // Should not error — returns however many bytes decompressed successfully.
        dec.read_to_end(&mut out).unwrap();
        assert!(
            flag.load(Ordering::Relaxed),
            "truncated stream should set flag"
        );
    }

    #[test]
    fn from_reader_is_truncated_false_by_default() {
        let r = HprofReader::from_reader(io::Cursor::new(header_blob(4))).unwrap();
        assert!(!r.is_truncated());
    }
}
