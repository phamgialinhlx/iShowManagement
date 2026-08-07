//! Every child process rmux spawns must be spawned without a console window.
//!
//! This is a rule about a *class* of call site rather than about one function,
//! which is why it is enforced over the source instead of at runtime. rmux spawns
//! `ssh` from a dozen places and adds more over time; on Windows each one that
//! forgets `no_console_window()` puts a `cmd`-looking window on screen, and
//! because the app polls — metrics every two seconds, the status watch every 1.5
//! — a single missed call site is not a cosmetic slip but a window flashing over
//! the operator's work forever. That is precisely how it was reported.
//!
//! The failure is also invisible to everyone who could catch it: it does not
//! exist on macOS or Linux, it is not a compile error, and no test that runs a
//! command can see it. So the check is a read of the code.
//!
//! It is deliberately syntactic. A cleverer check would need to run on Windows to
//! mean anything, and the thing being guarded is exactly whether somebody
//! remembered to type it.

use std::path::{Path, PathBuf};

/// Source trees that ship inside the desktop app.
const ROOTS: &[&str] = &["crates", "src-tauri/src"];

/// Places the rule does not apply, and why.
fn exempt(path: &Path) -> bool {
    let path = path.to_string_lossy().replace('\\', "/");

    // The agent is cross-compiled for the *target* and runs there. It is never a
    // child of the Windows GUI process, and its one Windows-specific spawn is a
    // deliberately detached daemon with its own creation flags.
    path.contains("/rmux-agent/")
        // Integration tests and benches are console programs already.
        || path.contains("/tests/")
        || path.contains("/benches/")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("src-tauri has a parent").to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `target/` is build output; walking it is slow and finds nothing.
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && !exempt(&path) {
            out.push(path);
        }
    }
}

/// Everything before `#[cfg(test)]`.
///
/// Unit tests spawn commands to assert on their output, and a test that does so
/// runs from `cargo test` — a console program — where there is no window to
/// suppress. Including them would force the flag into places it says nothing.
fn production_source(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(at) => &text[..at],
        None => text,
    }
}

#[test]
fn every_spawned_command_suppresses_the_console_window() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ROOTS {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(files.len() > 10, "found only {} source files — the walk is wrong", files.len());

    let mut offenders: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        let source = production_source(&text);
        let lines: Vec<&str> = source.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            if !line.contains("Command::new(") {
                continue;
            }
            checked += 1;

            // The builder may be chained across several lines, or configured
            // statement by statement through a `let mut`. Ten lines covers both
            // shapes in this workspace with room to spare.
            let window = lines[i..lines.len().min(i + 10)].join("\n");
            if !window.contains("no_console_window()") {
                let relative = file.strip_prefix(&root).unwrap_or(file);
                offenders.push(format!(
                    "{}:{} — {}",
                    relative.display().to_string().replace('\\', "/"),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    // If this drops to zero the walk has silently stopped finding anything, and a
    // green result would mean nothing at all.
    assert!(checked > 5, "only {checked} spawn sites found — the scan is not working");

    assert!(
        offenders.is_empty(),
        "these commands would open a console window on Windows. Add \
         `.no_console_window()` (rmux_transport::NoConsoleWindow):\n  {}",
        offenders.join("\n  ")
    );
}
