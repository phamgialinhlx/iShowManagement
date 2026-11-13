//! The `rmux` skill: how an agent inside an rmux session learns to drive the
//! `rmux` CLI (spawn another agent, prompt it, wait for it, read it back).
//!
//! The text is `assets/rmux-skill/SKILL.md`, embedded — the agent that installs
//! it is one static binary uploaded to hosts, so it must carry what it writes,
//! exactly as the pi status extension is carried.
//!
//! ## Never overwrite somebody's edits
//!
//! The file lands in the operator's own `~/.claude/skills`, which is theirs to
//! edit. So the installed copy carries a **marker line** with a hash of the
//! skill body, and an existing file is replaced only when
//!
//! 1. it carries the marker (it is ours, not a skill of the same name the
//!    operator wrote), **and**
//! 2. its body still hashes to the value in its own marker (nobody edited it
//!    since an rmux build wrote it).
//!
//! An edited copy keeps the stale marker hash, fails (2), and is left alone for
//! good. A missing file is written. An identical file is not touched at all, so
//! a watcher on the directory sees no churn on every spawn.

use std::path::PathBuf;

/// The line in the asset that the installer turns into a hashed marker.
const PLACEHOLDER: &str = "<!-- rmux-managed -->";
/// The installed marker's prefix; the hash follows and ` -->` closes it.
const MARKER_PREFIX: &str = "<!-- rmux-managed fnv=";

/// The skill as shipped, with the placeholder marker line.
pub fn skill_template() -> &'static str {
    include_str!("../assets/rmux-skill/SKILL.md")
}

/// FNV-1a — the same function `rmux-agent::provision::fingerprint` uses. Not a
/// security property: it only has to notice that a human changed the text.
fn fnv(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The file contents to install: the template with its placeholder replaced by
/// a marker that hashes the template itself.
pub fn rendered_skill() -> String {
    let template = skill_template();
    let marker = format!("{MARKER_PREFIX}{} -->", fnv(template.as_bytes()));
    template.replacen(PLACEHOLDER, &marker, 1)
}

/// Is `existing` an unmodified copy written by *some* rmux build?
///
/// Reverses [`rendered_skill`]: put the placeholder back where the marker is
/// and check the result hashes to the value the marker claims.
fn is_pristine_copy(existing: &str) -> bool {
    let Some(start) = existing.find(MARKER_PREFIX) else { return false };
    let rest = &existing[start + MARKER_PREFIX.len()..];
    let Some(end) = rest.find(" -->") else { return false };
    let claimed = &rest[..end];
    let marker_len = MARKER_PREFIX.len() + end + " -->".len();
    let mut restored = String::with_capacity(existing.len());
    restored.push_str(&existing[..start]);
    restored.push_str(PLACEHOLDER);
    restored.push_str(&existing[start + marker_len..]);
    fnv(restored.as_bytes()) == claimed
}

/// Where Claude Code looks for a user-level skill: `~/.claude/skills/rmux/SKILL.md`.
pub fn claude_skill_path(home: &str) -> String {
    format!("{}/.claude/skills/rmux/SKILL.md", home.trim_end_matches('/'))
}

/// What [`install_claude_skill`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInstall {
    /// No file was there; it was written.
    Written,
    /// An older pristine rmux copy was replaced with this build's.
    Updated,
    /// This build's copy was already there; nothing was touched.
    Current,
    /// A file is there that rmux did not write, or that someone edited. Left alone.
    KeptUserCopy,
}

/// Install the skill for Claude Code, idempotently and never over someone's edits.
///
/// Best-effort by design, like [`crate::pi::install_status_extension`]: a host
/// where it cannot be written still runs Claude, only without the skill.
pub fn install_claude_skill(home: &str) -> std::io::Result<SkillInstall> {
    let path = PathBuf::from(claude_skill_path(home));
    let wanted = rendered_skill();

    let outcome = match std::fs::read_to_string(&path) {
        Ok(existing) if existing == wanted => return Ok(SkillInstall::Current),
        Ok(existing) if is_pristine_copy(&existing) => SkillInstall::Updated,
        Ok(_) => return Ok(SkillInstall::KeptUserCopy),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SkillInstall::Written,
        Err(e) => return Err(e),
    };

    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    // Temp-then-rename, so Claude reading the directory mid-write never loads
    // half a skill.
    let temp = dir.join(format!(".SKILL.md.{}.tmp", std::process::id()));
    std::fs::write(&temp, wanted.as_bytes())?;
    std::fs::rename(&temp, &path)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_home(tag: &str) -> String {
        let home = std::env::temp_dir().join(format!("rmux-skill-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home.to_string_lossy().into_owned()
    }

    #[test]
    fn the_skill_keeps_its_frontmatter_first_and_carries_a_marker() {
        // Claude Code reads the YAML frontmatter only from the first line.
        let rendered = rendered_skill();
        assert!(rendered.starts_with("---\nname: rmux\n"), "{}", &rendered[..40]);
        assert!(rendered.contains(MARKER_PREFIX));
        assert!(!rendered.contains(PLACEHOLDER), "placeholder must be replaced");
        assert!(rendered.contains("RMUX_ENV"), "teaches the guard");
        assert!(rendered.contains("Apache-2.0"), "credits the herdr skill it adapts");
        assert!(is_pristine_copy(&rendered));
    }

    #[test]
    fn install_writes_then_leaves_a_current_copy_alone() {
        let home = scratch_home("idem");
        assert_eq!(install_claude_skill(&home).unwrap(), SkillInstall::Written);
        let path = claude_skill_path(&home);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(install_claude_skill(&home).unwrap(), SkillInstall::Current);
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after, "an identical copy is not rewritten");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_older_pristine_copy_is_upgraded() {
        let home = scratch_home("upgrade");
        // What an earlier build would have written: different text, valid marker.
        let old_template = "---\nname: rmux\n---\n\n<!-- rmux-managed -->\n\nold text\n";
        let old = old_template.replacen(
            PLACEHOLDER,
            &format!("{MARKER_PREFIX}{} -->", fnv(old_template.as_bytes())),
            1,
        );
        let path = claude_skill_path(&home);
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
        std::fs::write(&path, &old).unwrap();

        assert_eq!(install_claude_skill(&home).unwrap(), SkillInstall::Updated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), rendered_skill());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_edited_or_foreign_copy_is_never_overwritten() {
        let home = scratch_home("edited");
        let path = claude_skill_path(&home);
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();

        // Our copy, then the operator adds a line of their own.
        let edited = rendered_skill() + "\n- my own rule\n";
        std::fs::write(&path, &edited).unwrap();
        assert_eq!(install_claude_skill(&home).unwrap(), SkillInstall::KeptUserCopy);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

        // A skill called rmux that we never wrote at all.
        std::fs::write(&path, "---\nname: rmux\n---\nmine\n").unwrap();
        assert_eq!(install_claude_skill(&home).unwrap(), SkillInstall::KeptUserCopy);
        let _ = std::fs::remove_dir_all(&home);
    }
}
