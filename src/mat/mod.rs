mod codec;

use std::io;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub struct MatEmitter {
    dir: PathBuf,
    prefix: String,
}

#[allow(dead_code)]
impl MatEmitter {
    pub fn new(dir: &Path, prefix: &str) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
        })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}{}.index", self.prefix, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emitter_creates_dir_and_is_noop_without_calls() {
        let tmp = std::env::temp_dir().join("mat_emit_test_0");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
        drop(e);
        assert!(tmp.exists());
    }
}
