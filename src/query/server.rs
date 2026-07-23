//! Loopback HTTP server for programmatic OQL access (`query --server`).
//! POST OQL to `/` (raw body or {"query":"..."}), get a JSON QueryResult back;
//! GET /help returns the language reference. Loopback-only, sync tiny_http.

use std::io;

pub fn run_server(path: &str, path_depth: usize, port: u16) -> io::Result<()> {
    let _ = (path, path_depth, port);
    todo!("implemented in later tasks")
}
