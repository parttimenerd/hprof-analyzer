//! `hprof-redact` — standalone heap dump redactor.
//!
//! Zeroes all primitive field values and array element data in a Java heap dump
//! while preserving the complete object graph (class/field names, reference links,
//! dominator tree). The output is safe to share and remains readable by
//! hprof-analyzer, Eclipse MAT, and jhat.
//!
//! Usage:
//!   hprof-redact <INPUT> <OUTPUT>      # file → file
//!   hprof-redact - <OUTPUT>            # stdin → file  (input read into memory)
//!   hprof-redact <INPUT> -             # file  → stdout (raw .hprof only)
//!   hprof-redact - -                   # stdin → stdout
//!
//! Output compression is inferred from the OUTPUT extension:
//!   .hprof       raw (no compression)
//!   .hprof.gz    gzip
//!   .hprof.zip   zip (single entry "dump.hprof")
//!   -            raw bytes to stdout (no compression)
//!
//! INPUT can be .hprof, .hprof.gz, .hprof.zip, .tgz, or "-" for stdin.

use std::{
    env,
    io::{self, Read, Write},
    process,
};

use hprof_analyzer::{redact::redact, source::HprofSource};

fn usage() -> ! {
    eprintln!(
        "Usage: hprof-redact <INPUT> <OUTPUT>\n\
         \n\
         INPUT   path to .hprof/.hprof.gz/.hprof.zip, or '-' to read from stdin\n\
         OUTPUT  output path (.hprof / .hprof.gz / .hprof.zip), or '-' for stdout\n\
         \n\
         Zeroes all primitive field values and array contents while preserving\n\
         the object graph. Output is readable by hprof-analyzer, Eclipse MAT, jhat.\n\
         \n\
         Examples:\n\
         \n\
         hprof-redact dump.hprof redacted.hprof\n\
         hprof-redact dump.hprof.gz redacted.hprof.gz\n\
         cat dump.hprof | hprof-redact - redacted.hprof\n\
         hprof-redact dump.hprof - | gzip > redacted.hprof.gz"
    );
    process::exit(1)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        usage();
    }
    let input = &args[1];
    let output = &args[2];

    if let Err(e) = run(input, output) {
        eprintln!("hprof-redact: {e}");
        process::exit(1);
    }
}

fn run(input: &str, output: &str) -> io::Result<()> {
    let progress = |phase: &str, fraction: f64| {
        // Only print to stderr when writing to a file (stdout might be piped).
        if output != "-" {
            if fraction == 0.0 {
                eprintln!("{phase}…");
            } else if fraction == 1.0 {
                eprintln!("{phase} done");
            }
        }
    };

    // Build the HprofSource — stdin is buffered into memory so the two-pass
    // redactor can open it twice without seeking.
    let source: HprofSource = if input == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        HprofSource::from_bytes(buf, "stdin.hprof")
    } else {
        HprofSource::from(input)
    };

    let lower = output.to_ascii_lowercase();

    if output == "-" {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        redact(&source, &mut out, progress)?;
        out.flush()
    } else if lower.ends_with(".hprof.gz") {
        let file = std::fs::File::create(output)?;
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
        redact(&source, gz, progress)
    } else if lower.ends_with(".hprof.zip") {
        let file = std::fs::File::create(output)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("dump.hprof", opts)
            .map_err(io::Error::other)?;
        redact(&source, &mut zip, progress)?;
        zip.finish().map_err(io::Error::other)?;
        Ok(())
    } else {
        let file = std::fs::File::create(output)?;
        redact(&source, file, progress)
    }
}
