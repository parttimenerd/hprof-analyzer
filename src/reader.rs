use std::{
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
};
use flate2::read::GzDecoder;

pub struct HprofReader {
    pub format: String,
    pub id_size: u8,
    pub timestamp_ms: u64,
    inner: Box<dyn Read>,
}

impl HprofReader {
    pub fn open(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut buf = BufReader::new(file);
        let mut magic = [0u8; 2];
        buf.read_exact(&mut magic)?;
        let inner: Box<dyn Read> = if magic == [0x1f, 0x8b] {
            // gzip: re-open from start
            Box::new(GzDecoder::new(File::open(path)?))
        } else {
            // plain: prepend the 2 peeked bytes
            Box::new(io::Cursor::new(magic.to_vec()).chain(buf))
        };
        let mut r = HprofReader {
            format: String::new(),
            id_size: 4,
            timestamp_ms: 0,
            inner,
        };
        r.read_header()?;
        Ok(r)
    }

    fn read_header(&mut self) -> io::Result<()> {
        // null-terminated format string
        let mut s = Vec::new();
        loop {
            let b = self.u1()?;
            if b == 0 { break; }
            s.push(b);
        }
        self.format = String::from_utf8_lossy(&s).into_owned();
        // id_size stored as 4-byte big-endian int
        self.id_size = self.u4()? as u8;
        self.timestamp_ms = self.u8()?;
        Ok(())
    }

    pub fn u1(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.inner.read_exact(&mut b)?;
        Ok(b[0])
    }

    pub fn u2(&mut self) -> io::Result<u16> {
        let mut b = [0u8; 2];
        self.inner.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    pub fn u4(&mut self) -> io::Result<u32> {
        let mut b = [0u8; 4];
        self.inner.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }

    pub fn u8(&mut self) -> io::Result<u64> {
        let mut b = [0u8; 8];
        self.inner.read_exact(&mut b)?;
        Ok(u64::from_be_bytes(b))
    }

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

    pub fn skip(&mut self, mut n: u64) -> io::Result<()> {
        let mut buf = [0u8; 8192];
        while n > 0 {
            let chunk = n.min(8192) as usize;
            self.inner.read_exact(&mut buf[..chunk])?;
            n -= chunk as u64;
        }
        Ok(())
    }

    pub fn read_bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.inner.read_exact(&mut v)?;
        Ok(v)
    }

    /// Like `read_bytes` but reuses an existing buffer to avoid repeated allocation
    /// in hot loops. The buffer is resized to exactly `n` bytes before reading.
    pub fn read_bytes_reuse(&mut self, buf: &mut Vec<u8>, n: usize) -> io::Result<()> {
        buf.resize(n, 0);
        self.inner.read_exact(buf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP_PLAIN: &str = "/home/i560383/test-heapdumps/dump_0_fj-kmeans.hprof";
    const DUMP_GZ: &str =
        "/home/i560383/test-heapdumps/ArrayTest_84219_20260624_160147.hprof.gz";

    #[test]
    fn read_header_plain() {
        if !Path::new(DUMP_PLAIN).exists() {
            return;
        }
        let r = HprofReader::open(DUMP_PLAIN).unwrap();
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
        if !Path::new(DUMP_GZ).exists() {
            return;
        }
        let r = HprofReader::open(DUMP_GZ).unwrap();
        assert!(r.id_size == 4 || r.id_size == 8);
        assert!(r.format.starts_with("JAVA PROFILE"));
    }

    #[test]
    fn read_primitives() {
        // Build a tiny buffer: u1=0xAB, u2=0x1234, u4=0xDEADBEEF, u8=0x0102030405060708
        let data: Vec<u8> = vec![
            0xAB,
            0x12, 0x34,
            0xDE, 0xAD, 0xBE, 0xEF,
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        ];
        let mut r = HprofReader {
            format: String::new(),
            id_size: 4,
            timestamp_ms: 0,
            inner: Box::new(io::Cursor::new(data)),
        };
        assert_eq!(r.u1().unwrap(), 0xAB);
        assert_eq!(r.u2().unwrap(), 0x1234);
        assert_eq!(r.u4().unwrap(), 0xDEADBEEF);
        assert_eq!(r.u8().unwrap(), 0x0102030405060708);
    }
}
