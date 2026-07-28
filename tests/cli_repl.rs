//! CLI-surface tests for the interactive OQL REPL (`query <dump> --repl`). The
//! REPL is driven non-interactively by piping stdin. These use the small
//! committed philosophers fixture (LFS-gated); when the fixture is absent the
//! tests no-op, matching the pattern in `cli_query.rs` / `cli_unified.rs`.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_hprof-analyzer");

/// Locate the committed philosophers dump, or `None` when it is an unhydrated
/// LFS pointer (so CI without LFS still passes).
fn philosophers() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_4_philosophers.hprof",
        env!("CARGO_MANIFEST_DIR")
    );
    match std::fs::metadata(&p) {
        Ok(m) if m.len() >= 1024 => Some(p),
        _ => None,
    }
}

/// Run the REPL with the given stdin, returning (success, stdout).
fn run_repl(hprof: &str, stdin: &str) -> (bool, String) {
    let mut child = Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .arg("--repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// `!plan` shows the banner and a plan explanation without scanning the heap,
/// and `!quit` exits cleanly (exit 0).
///
/// Ignored in CI: reedline does not emit the startup banner when stdin is not a
/// TTY (piped), so the `stdout.contains("hprof-analyzer OQL REPL")` assertion
/// always fails in a headless environment.
#[test]
#[ignore = "reedline suppresses banner on non-TTY stdin; run manually"]
fn repl_plan_then_quit() {
    let Some(hprof) = philosophers() else { return };
    let (ok, stdout) = run_repl(&hprof, "!plan SELECT * FROM java.lang.String\n!quit\n");
    assert!(ok, "REPL should exit 0:\n{stdout}");
    assert!(
        stdout.contains("hprof-analyzer OQL REPL"),
        "banner missing:\n{stdout}"
    );
    assert!(
        stdout.contains("stage:"),
        "plan explanation missing:\n{stdout}"
    );
    assert!(
        stdout.contains("needs (armed):"),
        "plan needs missing:\n{stdout}"
    );
}

/// A bad query keeps the REPL alive (prints an error and reprompts, still exit
/// 0 on `!quit`).
#[test]
fn repl_bad_query_stays_alive() {
    let Some(hprof) = philosophers() else { return };
    let (ok, stdout) = run_repl(&hprof, "SELCT * FROM C\n!quit\n");
    assert!(ok, "REPL should survive a bad query and exit 0:\n{stdout}");
    assert!(
        stdout.contains("error:"),
        "expected an error line:\n{stdout}"
    );
}

/// An actual query line runs the full pipeline and prints a result row.
#[test]
fn repl_runs_real_query() {
    let Some(hprof) = philosophers() else { return };
    let (ok, stdout) = run_repl(&hprof, "SELECT COUNT(*) FROM java.lang.String\n!quit\n");
    assert!(ok, "REPL should exit 0:\n{stdout}");
    assert!(
        stdout.contains("COUNT(*)"),
        "missing COUNT header:\n{stdout}"
    );
    assert!(
        stdout.contains("1 row"),
        "missing row-count line:\n{stdout}"
    );
}
