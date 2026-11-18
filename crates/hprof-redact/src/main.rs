//! `hprof-redact` — standalone heap dump redactor.
//!
//! Zeroes primitive array elements (lean, default) or all primitive values
//! (complete) in a Java heap dump while preserving the complete object graph
//! (class/field names, reference links, dominator tree). The output is safe
//! to share and remains readable by hprof-analyzer, Eclipse MAT, and jhat.
//! With `--no-marker` the redaction marker record is omitted, so the output
//! also passes strict third-party parsers (e.g. hprof-slurp) — at the cost of
//! losing the machine-readable "this dump is redacted" marker.
//!
//! Usage:
//!   hprof-redact <INPUT> <OUTPUT>                       # lean mode (default)
//!   hprof-redact --complete <INPUT> <OUTPUT>            # complete (two-pass) mode
//!   hprof-redact --no-marker <INPUT> <OUTPUT>           # lean, no marker record
//!   hprof-redact --complete --no-marker <INPUT> <OUTPUT>
//!   hprof-redact --check <INPUT>                        # check if already redacted
//!   hprof-redact - <OUTPUT>                             # stdin → file
//!   hprof-redact <INPUT> -                              # file  → stdout (raw .hprof only)
//!   hprof-redact - -                                    # stdin → stdout
//!
//! Output compression is inferred from the OUTPUT extension:
//!   .hprof       raw (no compression)
//!   .gz          gzip (.hprof.gz and plain .gz)
//!   .hprof.zip   zip (single entry "dump.hprof")
//!   -            raw bytes to stdout (no compression)
//!
//! INPUT can be .hprof, .hprof.gz, .hprof.zip, .tar, .tar.gz, .tgz, or "-" for stdin.

use std::{
    io::{self, Read, Write},
    process,
};

use clap::Parser;
use hprof_analyzer::{
    Pass1,
    redact::{RedactMode, redact},
    source::HprofSource,
};

#[derive(Parser)]
#[command(
    name = "hprof-redact",
    version,
    about = "Standalone heap dump redactor: zeroes primitive values, preserves the object graph"
)]
struct Cli {
    /// Two-pass mode: also zeros scalar fields on instances and static fields on
    /// classes. Default is lean (single pass, arrays only).
    #[arg(long)]
    complete: bool,

    /// Don't embed the redaction marker record (tag 0xDE). Strict third-party
    /// parsers reject the unknown tag (hprof-slurp exits with "unhandled record
    /// tag 222"); the output then also passes such parsers, but hprof-analyzer
    /// shows no "Redacted dump" banner and --check reports the dump as not
    /// redacted.
    #[arg(long)]
    no_marker: bool,

    /// Check whether INPUT is already redacted. Exits 0 if redacted, 1 if not,
    /// 2 on error. No OUTPUT required.
    #[arg(long, requires = "input", conflicts_with = "output")]
    check: bool,

    /// Input path (.hprof, .hprof.gz, .hprof.zip, .tar, .tar.gz, .tgz) or '-' for stdin
    #[arg(value_name = "INPUT", required_unless_present = "check")]
    input: Option<String>,

    /// Output path (.hprof / .gz / .hprof.zip), or '-' for stdout
    #[arg(value_name = "OUTPUT", required_unless_present = "check")]
    output: Option<String>,
}

fn run_check(input: &str) {
    let source: HprofSource = if input == "-" {
        let mut buf = Vec::new();
        if let Err(e) = io::stdin().read_to_end(&mut buf) {
            eprintln!("hprof-redact: {e}");
            process::exit(2);
        }
        HprofSource::from_bytes(buf, "stdin.hprof")
    } else {
        HprofSource::from(input)
    };
    match Pass1::run(&source, false) {
        Ok(p1) => {
            if p1.redacted {
                println!("redacted");
                process::exit(0);
            } else {
                println!("not redacted");
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("hprof-redact: {e}");
            process::exit(2);
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if cli.check {
        run_check(cli.input.as_deref().expect("--check requires INPUT"));
        return;
    }
    let input = cli.input.expect("INPUT is required without --check");
    let output = cli.output.expect("OUTPUT is required without --check");
    let mode = if cli.complete {
        RedactMode::Complete
    } else {
        RedactMode::Lean
    };
    if let Err(e) = run(&input, &output, mode, cli.no_marker) {
        eprintln!("hprof-redact: {e}");
        process::exit(1);
    }
}

fn run(input: &str, output: &str, mode: RedactMode, no_marker: bool) -> io::Result<()> {
    let progress = |_phase: &str, _fraction: f64| {};

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

    let stats = if output == "-" {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let stats = redact(&source, &mut out, mode, !no_marker, progress)?;
        out.flush()?;
        stats
    } else if lower.ends_with(".gz") {
        let file = std::fs::File::create(output)?;
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
        redact(&source, gz, mode, !no_marker, progress)?
    } else if lower.ends_with(".hprof.zip") || lower.ends_with(".zip") {
        let file = std::fs::File::create(output)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("dump.hprof", opts)
            .map_err(io::Error::other)?;
        let stats = redact(&source, &mut zip, mode, !no_marker, progress)?;
        zip.finish().map_err(io::Error::other)?;
        stats
    } else {
        let file = std::fs::File::create(output)?;
        redact(&source, file, mode, !no_marker, progress)?
    };

    if stats.unknown_class_instances > 0 {
        eprintln!(
            "\x1b[1;33m⚠ {} instance(s) had no CLASS_DUMP in the file — their bodies were \
             copied verbatim (primitive values not redacted). This should not occur in a \
             valid HPROF.\x1b[0m",
            stats.unknown_class_instances
        );
    }
    if no_marker {
        eprintln!(
            "note: no redaction marker embedded — hprof-analyzer will not show the \
             'Redacted dump' banner and `--check` reports this dump as not redacted"
        );
    }
    Ok(())
}
