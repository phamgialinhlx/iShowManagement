//! The log file, and getting it out of the app.
//!
//! ## Why a file at all
//!
//! Everything rmux logs went to stdout, which on a double-clicked `.app` goes
//! nowhere a person can reach — and on Windows, where the app is launched from
//! Explorer, there is not even a console attached. So when something went wrong
//! on someone else's machine the only evidence was their description of it.
//! A file means "send me the log" is an instruction anyone can follow.
//!
//! ## It is capped, and rotated once
//!
//! A log that grows forever eventually fills a disk, and one that is wiped on
//! every launch is empty for exactly the crash you wanted to read about — the
//! app restarts before anyone thinks to look. So the current file is capped and
//! the previous run's is kept beside it: two files, bounded, and the interesting
//! one survives a restart.
//!
//! ## The export carries the context, not just the lines
//!
//! "It doesn't work on Windows" needs the OS build, the app version and which
//! agent binaries shipped before the log lines mean anything. The header is
//! assembled here rather than asked for, because the person reporting a bug is
//! the least able to answer those questions.

use std::sync::{Arc, Mutex};

use tauri::Manager;

/// The bundle identifier, duplicated from `tauri.conf.json`.
///
/// Needed before an `AppHandle` exists — the lines written during startup are
/// often the ones worth having. A test reads the manifest and pins the two
/// together, because a silent divergence would put the log somewhere the export
/// button does not look.
pub const IDENTIFIER: &str = "group.yitec.rmux";

/// Largest the live log is allowed to get before it is rotated.
///
/// Small on purpose. This is read by a person, and gets attached to a message —
/// a 200 MB log is one nobody sends and nobody opens.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// A file every `tracing` line is mirrored into.
#[derive(Clone)]
pub struct LogFile {
    handle: Arc<Mutex<std::fs::File>>,
}

impl LogFile {
    fn open(dir: &std::path::Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("rmux.log");

        // Rotate *before* opening, so this run starts with room and the previous
        // run's tail — usually the interesting part — is kept rather than
        // truncated away by the restart that followed it.
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            let _ = std::fs::rename(&path, dir.join("rmux.previous.log"));
        }

        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { handle: Arc::new(Mutex::new(file)) })
    }
}

impl std::io::Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // A poisoned lock must not take logging down with it — a panic in one
        // thread should not silence the record of *why* it panicked.
        match self.handle.lock() {
            Ok(mut file) => file.write(buf),
            Err(poisoned) => poisoned.into_inner().write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.handle.lock() {
            Ok(mut file) => file.flush(),
            Err(poisoned) => poisoned.into_inner().flush(),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogFile {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Where the logs live, given the app's data directory.
pub fn dir(app_data: &std::path::Path) -> std::path::PathBuf {
    app_data.join("logs")
}

/// Open the log file for this run. `None` if it cannot be created — logging to
/// a file is a convenience, and failing to start over it would be absurd.
pub fn writer(app_data: &std::path::Path) -> Option<LogFile> {
    LogFile::open(&dir(app_data)).ok()
}

/// What the UI needs to describe the log without reading it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStatus {
    pub path: String,
    pub bytes: u64,
    /// Whether a previous run's log is kept beside it.
    pub has_previous: bool,
}

#[tauri::command]
pub fn log_status<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<LogStatus, String> {
    let dir = dir(&app.path().app_data_dir().map_err(|e| e.to_string())?);
    let path = dir.join("rmux.log");

    Ok(LogStatus {
        bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        has_previous: dir.join("rmux.previous.log").exists(),
        path: path.to_string_lossy().into_owned(),
    })
}

/// Write a single shareable file and return its path.
///
/// One file, not a folder: the operator is going to attach this to a message,
/// and "zip these two and send them" is a step people get wrong. The previous
/// run is concatenated in, oldest first, under a header that says which is which.
#[tauri::command]
pub fn log_export<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<String, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let dir = dir(&app_data);

    // Written where a person can find it. The desktop is the one directory
    // everyone can locate under pressure; the app's own data directory is
    // buried, which is the whole problem being solved.
    let out_dir = dirs::desktop_dir()
        .or_else(dirs::download_dir)
        .or_else(dirs::home_dir)
        .unwrap_or(app_data.clone());

    let name = format!("rmux-log-{}.txt", stamp());
    let out = out_dir.join(&name);

    let mut text = header(&app);
    for (label, file) in [("previous run", "rmux.previous.log"), ("this run", "rmux.log")] {
        let path = dir.join(file);
        let Ok(body) = std::fs::read_to_string(&path) else { continue };
        text.push_str(&format!("\n===== {label} ({file}) =====\n\n"));
        text.push_str(&body);
    }

    std::fs::write(&out, text).map_err(|e| format!("could not write {}: {e}", out.display()))?;
    Ok(out.to_string_lossy().into_owned())
}

/// The context a log line is meaningless without.
fn header<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> String {
    let agents = app
        .path()
        .resource_dir()
        .ok()
        .map(|r| r.join("agents"))
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "none found".to_owned());

    format!(
        "rmux log export\n\
         version:    {}\n\
         platform:   {} {}\n\
         exported:   {}\n\
         agents:     {}\n",
        app.package_info().version,
        std::env::consts::OS,
        std::env::consts::ARCH,
        stamp(),
        agents,
    )
}

/// A filename-safe timestamp, without pulling in a date crate.
fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Civil-from-days, the same arithmetic `claude_account` already uses — this
    // has to be readable in a filename, not correct to the second in a timezone.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identifier_matches_the_manifest() {
        // `run()` resolves the log directory from this constant before any
        // `AppHandle` exists, while `log_export` asks Tauri. If they ever
        // disagree the app writes its log to one place and the export button
        // reads another — and reports an empty log for a run that logged
        // plenty.
        let manifest = include_str!("../tauri.conf.json");
        let line = manifest
            .lines()
            .find(|l| l.contains("\"identifier\""))
            .expect("no identifier in tauri.conf.json");
        assert!(line.contains(IDENTIFIER), "manifest says {line}, code says {IDENTIFIER}");
    }

    #[test]
    fn the_stamp_is_filename_safe_and_sorts() {
        let s = stamp();
        assert!(!s.contains(':'), "a colon is not a filename on Windows: {s}");
        assert!(!s.contains(' '), "{s}");
        // `YYYYMMDD-HHMMSS`, so a directory listing is chronological.
        assert_eq!(s.len(), 15, "{s}");
        assert_eq!(s.as_bytes()[8], b'-', "{s}");
    }

    #[test]
    fn a_log_is_rotated_rather_than_truncated() {
        // The failure this prevents: the app restarts after a crash, wipes the
        // file, and the one run anybody wanted to read is gone before they
        // thought to look.
        let dir = std::env::temp_dir().join(format!("rmux-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("rmux.log");
        std::fs::write(&path, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();

        let _log = LogFile::open(&dir).unwrap();
        assert!(dir.join("rmux.previous.log").exists(), "the old log was discarded");
        assert!(std::fs::metadata(&path).unwrap().len() < MAX_BYTES, "not rotated");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_small_log_is_left_alone() {
        // Rotating on every launch would leave the previous file holding one
        // line and throw away the run before it.
        let dir = std::env::temp_dir().join(format!("rmux-log-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rmux.log"), b"one line\n").unwrap();

        let _log = LogFile::open(&dir).unwrap();
        assert!(!dir.join("rmux.previous.log").exists());
        assert!(std::fs::read_to_string(dir.join("rmux.log")).unwrap().contains("one line"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
