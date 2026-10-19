//! Helpers for building and opening a seekable gz block index via `indexed_deflate`.
//!
//! `build` does a one-time sequential pass over the gz file and writes an index
//! to `index_path`; `open` returns a `GzDecoder` that implements `Read + Seek`
//! on the decompressed stream using that index.

use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

use indexed_deflate::{AccessPointSpan, GzDecoder, GzIndexBuilder};

/// Build a gz block index for `gz_path`, writing it to `index_path`.
///
/// Performs a full sequential decompression pass — can take minutes for large
/// files. Call only from `heap load` (not from interactive sessions).
pub fn build(gz_path: &Path, index_path: &Path) -> io::Result<()> {
    let src = File::open(gz_path)?;
    let idx = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(index_path)?;
    let mut builder = GzIndexBuilder::new(src, idx, AccessPointSpan::default())
        .map_err(|e| io::Error::other(e.to_string()))?;
    io::copy(&mut builder, &mut io::sink())?;
    builder
        .finish()
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Open a seekable gz decoder for `gz_path` using its pre-built block index at
/// `index_path`. The returned decoder implements `Read + Seek` on the
/// decompressed stream.
#[allow(dead_code)]
pub fn open(gz_path: &Path, index_path: &Path) -> io::Result<GzDecoder<File, File>> {
    GzDecoder::new(File::open(gz_path)?, File::open(index_path)?)
        .map_err(|e| io::Error::other(e.to_string()))
}
