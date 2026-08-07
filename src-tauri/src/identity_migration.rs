//! Carry the operator's data across a change of bundle identifier.
//!
//! ## The mistake this exists to stop repeating
//!
//! Renaming the identifier (`ai.betterscale.rmux` → `group.yitec.rmux`) reads
//! like a cosmetic change and is not. macOS keys **per-application storage** by
//! bundle identifier, so on the first launch after the rename the app is, as far
//! as the system is concerned, a program that has never run:
//!
//! - `~/Library/WebKit/<id>/` — the WKWebView data store, which holds
//!   **`localStorage`**. That is where rmux keeps the entire workspace: servers,
//!   projects, the session list, notes, the activity tally, goals, Jira
//!   selections and shortcuts.
//! - `~/Library/Application Support/<id>/` — background pictures, downloaded
//!   face models, the log file.
//!
//! Measured on a real machine, and reported by the operator as "oh no i lost all
//! my session": the sessions themselves were fine — they run under `rmux-agent`
//! on their hosts and never stopped — but rmux's *record* of them was in the old
//! directory, so the rail came up empty and the work became unreachable from the
//! app. The keychain was migrated (see `keychain`) and the webview store was
//! not, which is the more damaging of the two.
//!
//! ## Copy, never move
//!
//! The old directories are left exactly as they are. If anything here is wrong —
//! a partial copy, an unreadable database, a version of the app that predates
//! this code — the operator can still open the previous build and find their
//! work. A migration that destroys its own source has one chance to be correct.
//!
//! ## It runs once, before the window exists
//!
//! WebKit opens `localstorage.sqlite3` when the webview is created and holds it
//! with a WAL. Copying into a store that is already open is how you get a
//! database that is present, non-empty, and missing the last write. So this runs
//! during setup, before any window is built, and does nothing at all if the
//! destination already has content.

use std::path::Path;

/// Identifiers this app has previously shipped under, newest first.
///
/// A list rather than one name, because the next rename must not silently skip
/// anyone who is still two versions back. Order matters only in that the first
/// directory found with real content wins.
const PREVIOUS_IDENTIFIERS: &[&str] = &["ai.betterscale.rmux"];

/// What a migration attempt did, for the log and for tests.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The destination already had data — nothing was touched.
    AlreadyPresent,
    /// No previous install was found.
    NothingToMigrate,
    Migrated { from: String },
}

/// Copy webview storage and application support from a previous identifier.
///
/// Called before the first window is created. Failures are logged and swallowed:
/// starting with an empty workspace is bad, and refusing to start at all is
/// worse.
pub fn run(current: &str) -> Outcome {
    let Some(home) = dirs::home_dir() else {
        return Outcome::NothingToMigrate;
    };
    let roots = [home.join("Library/WebKit"), home.join("Library/Application Support")];

    // Both directories move together or the app ends up half-migrated — a
    // workspace with no background, or worse, a background setting pointing at a
    // file that is no longer there.
    for previous in PREVIOUS_IDENTIFIERS {
        if roots.iter().all(|root| !has_content(&root.join(previous))) {
            continue;
        }
        // The destination having content means this has already run, or the
        // operator has used the new build. Either way, theirs wins.
        if roots.iter().any(|root| has_content(&root.join(current))) {
            return Outcome::AlreadyPresent;
        }

        for root in &roots {
            let from = root.join(previous);
            let to = root.join(current);
            if !has_content(&from) {
                continue;
            }
            if let Err(e) = copy_tree(&from, &to) {
                tracing::warn!(error = %e, from = %from.display(), "could not migrate app data");
            }
        }
        tracing::info!(from = previous, to = current, "migrated app data from a previous identifier");
        return Outcome::Migrated { from: (*previous).to_string() };
    }

    Outcome::NothingToMigrate
}

/// Does this directory hold anything worth carrying over?
///
/// An *existing but empty* directory is the normal state of a fresh install —
/// macOS creates it eagerly — so a plain `exists()` would report every new
/// machine as having data to preserve, and then refuse to migrate onto it.
fn has_content(dir: &Path) -> bool {
    std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// Recursive copy that does not overwrite anything already at the destination.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() && !target.exists() {
            // Symlinks are skipped rather than followed: a store can contain
            // links out of the tree, and copying through one would write
            // somewhere nobody asked for.
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A directory of this test's own.
    ///
    /// Named from an atomic counter rather than a timestamp. The first version
    /// used nanoseconds and failed under `cargo test`'s default parallelism
    /// while passing with `--test-threads=1` — two threads entering within the
    /// same tick shared a directory and deleted each other's fixtures. A test
    /// that passes alone and fails alongside its neighbours is the worst kind,
    /// because the obvious reading is that the code is flaky.
    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rmux-migration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_empty_directory_is_not_content() {
        let dir = temp();
        // macOS creates these eagerly, so treating "exists" as "has data" would
        // make every fresh install refuse its own migration.
        assert!(!has_content(&dir));
        std::fs::write(dir.join("a"), b"x").unwrap();
        assert!(has_content(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tree_is_copied_whole() {
        let root = temp();
        let from = root.join("old");
        std::fs::create_dir_all(from.join("WebsiteData/Default/hash/LocalStorage")).unwrap();
        std::fs::write(from.join("WebsiteData/Default/hash/LocalStorage/db.sqlite3"), b"sessions").unwrap();

        let to = root.join("new");
        copy_tree(&from, &to).unwrap();
        assert_eq!(
            std::fs::read(to.join("WebsiteData/Default/hash/LocalStorage/db.sqlite3")).unwrap(),
            b"sessions"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_source_is_left_alone() {
        let root = temp();
        let from = root.join("old");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("keep"), b"x").unwrap();

        copy_tree(&from, &root.join("new")).unwrap();
        // Copy, never move: if any of this is wrong, the previous build must
        // still be able to open the operator's work.
        assert!(from.join("keep").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_files_are_never_overwritten() {
        let root = temp();
        let from = root.join("old");
        let to = root.join("new");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("f"), b"old").unwrap();
        std::fs::write(to.join("f"), b"new").unwrap();

        copy_tree(&from, &to).unwrap();
        // Whatever the operator did with the new build wins. Clobbering it would
        // turn a migration into data loss in the other direction.
        assert_eq!(std::fs::read(to.join("f")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&root);
    }
}
