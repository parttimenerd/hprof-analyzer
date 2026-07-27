use std::io::{self, Cursor};
use std::sync::Arc;

use flate2::read::GzDecoder;

use crate::reader::HprofReader;

/// Origin of an HPROF byte stream — either a filesystem path or an in-memory
/// buffer. All parse passes take `&HprofSource` instead of `path: &str` and
/// call `source.open()` to start a new sequential scan.
///
/// Inner helper functions never see `HprofSource` — they take either a
/// pre-opened `&mut HprofReader` (single-scan) or an
/// `impl Fn() -> io::Result<HprofReader>` opener closure (multi-scan).
#[derive(Clone)]
pub enum HprofSource {
    /// A filesystem path (CLI / native use).
    Path(String),
    /// An in-memory buffer (WASM / test use).
    Bytes { data: Arc<[u8]>, name: String },
}

impl HprofSource {
    /// Open a new sequential reader positioned at the start of the stream.
    /// Gzip is auto-detected for both variants.
    pub fn open(&self) -> io::Result<HprofReader> {
        match self {
            HprofSource::Path(p) => HprofReader::open(p),
            HprofSource::Bytes { data, .. } => {
                let bytes = Arc::clone(data);
                if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
                    HprofReader::from_reader(GzDecoder::new(Cursor::new(bytes)))
                } else {
                    HprofReader::from_reader(Cursor::new(bytes))
                }
            }
        }
    }

    /// Total byte length — exact for `Bytes`, from `fs::metadata` for `Path`.
    pub fn len(&self) -> io::Result<u64> {
        match self {
            HprofSource::Path(p) => Ok(std::fs::metadata(p)?.len()),
            HprofSource::Bytes { data, .. } => Ok(data.len() as u64),
        }
    }

    /// Human-readable name: basename for `Path`, `name` field for `Bytes`.
    pub fn display_name(&self) -> &str {
        match self {
            HprofSource::Path(p) => std::path::Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(p),
            HprofSource::Bytes { name, .. } => name,
        }
    }

    /// Full path string for `Path`; same as `display_name` for `Bytes`.
    pub fn file_path(&self) -> &str {
        match self {
            HprofSource::Path(p) => p,
            HprofSource::Bytes { name, .. } => name,
        }
    }
}

impl From<&str> for HprofSource {
    fn from(p: &str) -> Self {
        HprofSource::Path(p.to_string())
    }
}

impl From<String> for HprofSource {
    fn from(p: String) -> Self {
        HprofSource::Path(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_display_name_is_basename() {
        let s = HprofSource::from("/tmp/some/heap.hprof");
        assert_eq!(s.display_name(), "heap.hprof");
        assert_eq!(s.file_path(), "/tmp/some/heap.hprof");
    }

    #[test]
    fn bytes_display_name_is_name_field() {
        let s = HprofSource::Bytes {
            data: Arc::from(b"dummy".as_ref()),
            name: "my.hprof".to_string(),
        };
        assert_eq!(s.display_name(), "my.hprof");
        assert_eq!(s.file_path(), "my.hprof");
    }

    #[test]
    fn path_len_returns_file_size() {
        // Use a fixture that is always present.
        let s = HprofSource::from("tests/fixtures/dump_2_scala-doku.hprof");
        let len = s.len().unwrap();
        assert!(len > 0, "fixture should have non-zero length");
    }

    #[test]
    fn bytes_len_returns_data_len() {
        let data: Arc<[u8]> = Arc::from(vec![1u8, 2, 3, 4, 5].as_slice());
        let s = HprofSource::Bytes { data, name: "x.hprof".to_string() };
        assert_eq!(s.len().unwrap(), 5);
    }
}
