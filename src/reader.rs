//! Streaming byte reader that fronts the HPROF dump file. Transparently
//! decompresses gzip'd dumps (magic sniff) and serves the big-endian
//! `u1`/`u2`/`u4`/`u8`/`id` primitives the parser consumes, buffering in large
//! chunks so a multi-gigabyte scan stays sequential and allocation-light.

use flate2::read::GzDecoder;
use std::{
    fs::File,
    io::{self, BufReader, Read},
};

const BUF_CAP: usize = 1 << 20; // 1 MiB refill chunk

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
}

impl HprofReader {
    /// Open a dump (gzip auto-detected via magic) and consume its HPROF header.
    pub fn open(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut peek = BufReader::new(file);
        let mut magic = [0u8; 2];
        peek.read_exact(&mut magic)?;
        let inner: Box<dyn Read> = if magic == [0x1f, 0x8b] {
            Box::new(GzDecoder::new(File::open(path)?))
        } else {
            Box::new(io::Cursor::new(magic.to_vec()).chain(peek))
        };
        let mut r = HprofReader {
            format: String::new(),
            id_size: 4,
            timestamp_ms: 0,
            inner,
            buf: vec![0u8; BUF_CAP],
            pos: 0,
            end: 0,
        };
        r.read_header()?;
        Ok(r)
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
        self.id_size = self.u4()? as u8;
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
        Ok(b)
    }

    /// Read a big-endian `u16`.
    #[inline]
    pub fn u2(&mut self) -> io::Result<u16> {
        self.ensure(2)?;
        let p = self.pos;
        let v = u16::from_be_bytes([self.buf[p], self.buf[p + 1]]);
        self.pos = p + 2;
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
        while n > 0 {
            let avail = self.end - self.pos;
            if avail == 0 {
                if self.refill()? == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof in skip"));
                }
                continue;
            }
            let take = (avail as u64).min(n) as usize;
            self.pos += take;
            n -= take as u64;
        }
        Ok(())
    }

    /// Read exactly `n` bytes into a freshly allocated `Vec`.
    pub fn read_bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.read_into(&mut v)?;
        Ok(v)
    }

    /// Like `read_bytes` but reuses an existing buffer to avoid repeated allocation.
    pub fn read_bytes_reuse(&mut self, buf: &mut Vec<u8>, n: usize) -> io::Result<()> {
        buf.resize(n, 0);
        self.read_into(buf)
    }

    /// Fill `dst` completely from the internal buffer + underlying stream.
    fn read_into(&mut self, dst: &mut [u8]) -> io::Result<()> {
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
                    self.inner.read_exact(&mut dst[written..])?;
                    written = dst.len();
                } else if self.refill()? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "eof in read_into",
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
        };
        assert_eq!(r.u1().unwrap(), 0);
        r.skip(9).unwrap(); // skip 1..=9
        assert_eq!(r.u1().unwrap(), 10);
        let mut buf = Vec::new();
        r.read_bytes_reuse(&mut buf, 5).unwrap();
        assert_eq!(buf, vec![11, 12, 13, 14, 15]);
    }
}
