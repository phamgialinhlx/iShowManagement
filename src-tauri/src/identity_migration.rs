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
    let Some(roots) = storage_roots() else {
        return Outcome::NothingToMigrate;
    };
    let roots = roots.as_slice();

    // Both directories move together or the app ends up half-migrated — a
    // workspace with no background, or worse, a background setting pointing at a
    // file that is no longer there.
    for previous in PREVIOUS_IDENTIFIERS {
        if roots.iter().all(|root: &std::path::PathBuf| !has_content(&root.join(previous))) {
            continue;
        }
        // The destination having content means this has already run, or the
        // operator has used the new build. Either way, theirs wins.
        if roots.iter().any(|root| has_content(&root.join(current))) {
            return Outcome::AlreadyPresent;
        }

        for root in roots {
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

/// The directories this platform keys by bundle identifier.
///
/// **The only platform-specific part of this module**, deliberately: everything
/// below — what counts as content, the copy, the refusal to overwrite — is the
/// same problem everywhere, and the rename costs an operator their whole
/// workspace on any OS that namespaces storage by identifier. All of them do.
///
/// The addresses are measured on each platform rather than taken from the docs,
/// because the webview store is the one that matters and it is not where the
/// app's *own* data lives:
///
/// - **macOS**: `~/Library/WebKit/<id>/` is the WKWebView store holding
///   `localStorage`; `~/Library/Application Support/<id>/` holds backgrounds,
///   face models and the log.
/// - **Windows**: `%LOCALAPPDATA%\<id>\` contains `EBWebView\`, which is
///   WebView2's user data folder and therefore `localStorage`; `%APPDATA%\<id>\`
///   holds the rest. Confirmed on a real machine mid-upgrade: both existed under
///   `ai.betterscale.rmux`, with `EBWebView` and `logs` inside them and 14
///   `rmux.` keys in the leveldb.
/// - **Linux**: `~/.local/share/<id>/` and `~/.config/<id>/`, where WebKitGTK
///   and Tauri put them.
///
/// macOS keeps `dirs::home_dir()` joins rather than `data_local_dir()`, which
/// would resolve to `~/Library/Application Support` for *both* entries and
/// migrate half the data. Returning `None` means "this platform does not
/// namespace by identifier", which the caller reads as nothing to migrate.
fn storage_roots() -> Option<Vec<std::path::PathBuf>> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir()?;
        Some(vec![home.join("Library/WebKit"), home.join("Library/Application Support")])
    }

    // `data_local_dir` is `%LOCALAPPDATA%` and `config_dir` is `%APPDATA%` here,
    // which is the pair Tauri derives the app's own directories from — so these
    // are the same roots the running app will use under the new name.
    #[cfg(not(target_os = "macos"))]
    {
        Some(vec![dirs::data_local_dir()?, dirs::config_dir()?])
    }
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

    /// This platform has somewhere to migrate from and to.
    ///
    /// `run` is a no-op the moment this returns `None` or the wrong pair, and it
    /// is a **silent** one — the app starts with an empty rail and the operator
    /// reads it as having lost their sessions, which is exactly how the macOS
    /// half was reported ("oh no i lost all my session").
    ///
    /// The roots were macOS paths unconditionally, so on Windows and Linux this
    /// module found nothing and every user upgrading past the rename would have
    /// lost servers, projects, the session list, notes and shortcuts — with the
    /// sessions still running on their hosts and no longer reachable from the
    /// app. The tests below only exercised the copy helpers, which are platform
    /// independent and were never the broken part.
    #[test]
    fn this_platform_has_storage_roots() {
        let roots = storage_roots().expect("every supported platform namespaces by identifier");
        assert_eq!(roots.len(), 2, "the webview store and the app data both move");
        for root in &roots {
            assert!(root.is_absolute(), "{root:?} must be absolute");
        }
        // Two names for one directory would copy the first onto the second and
        // migrate half the data. `data_local_dir()` on macOS resolves to
        // Application Support, which is why that platform is spelled out.
        assert_ne!(roots[0], roots[1], "the two roots must be distinct");
    }

    /// The roots are real directories on *this* machine.
    ///
    /// This is the assertion that actually catches wrong addresses, and getting
    /// it right took two attempts. The first version looked for a previous
    /// install and skipped when it found none — but it asked `storage_roots()`
    /// where to look, so wrong roots found nothing and the test **skipped
    /// instead of failing**. Verified: with the macOS paths restored on Windows
    /// it passed, green, while the migration silently did nothing. A check whose
    /// escape hatch is computed from the thing under test cannot fail.
    ///
    /// Existence is independent of that, and it is exactly what separates a real
    /// root from a plausible-looking one: `%LOCALAPPDATA%` is always there on
    /// Windows, `~/Library` is always there on macOS, and neither exists on the
    /// other. The parent is accepted because `~/Library/WebKit` appears only
    /// once a WebKit app has run, while `~/Library` is unconditional.
    #[test]
    fn the_roots_exist_on_this_machine() {
        let roots = storage_roots().expect("storage roots");
        for root in &roots {
            let usable = root.is_dir() || root.parent().is_some_and(Path::is_dir);
            assert!(
                usable,
                "{root:?} is not a directory on this machine and neither is its \
                 parent — these are another platform's paths, so the migration \
                 would silently find nothing and the operator would open an empty \
                 rail"
            );
        }
    }

    /// A previous install on this machine is found by the roots, not missed.
    ///
    /// Only meaningful on a machine mid-upgrade, and it says so rather than
    /// skipping silently. Kept alongside the existence check because it is the
    /// stronger statement when it can be made: not just "these directories are
    /// real" but "the data is where we are looking".
    #[test]
    fn a_previous_install_is_found_where_it_lives() {
        let roots = storage_roots().expect("storage roots");
        let old = PREVIOUS_IDENTIFIERS[0];

        if !roots.iter().any(|root| root.join(old).exists()) {
            eprintln!("skipped: no {old} install on this machine");
            return;
        }
        assert!(
            roots.iter().any(|root| has_content(&root.join(old))),
            "found {old} under one of {roots:?} but it looks empty — the roots \
             and the content check disagree about where the data is"
        );
    }

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
