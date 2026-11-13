//! Which credential set a child session starts on.
//!
//! `rmux spawn` (and `rmux new` from inside a session) used to start children
//! on the daemon's environment — not their parent's profile — so a Claude
//! session running on a work account spawned a helper that ran on whatever the
//! host defaulted to. Now the child is bound to the parent's set, and to the
//! parent's **chain with its active member and exhausted members**, so a child
//! does not start on an account its parent already spent.
//!
//! The rules, in order:
//!
//! 1. `--profile ID` names the set outright (`default` and `none` included).
//! 2. A child of the parent's own kind inherits the parent's binding.
//! 3. A child of a different kind (a Claude parent spawning Codex) uses that
//!    kind's **default set** (Settings › Models › default, synced to the host).
//!    If none is loaded the spawn is **refused**, naming why: the parent chose
//!    a provider, and quietly running the child on the host's own login is a
//!    provider nobody chose.
//! 4. A parent bound to `none` made no provider choice, so a child of another
//!    kind gets `none` too (its own host login).
//! 5. A parent with no binding at all (outside rmux, a plain shell, a session
//!    from an older client) gives no binding: the child behaves as before.

use rmux_claude_core::env_kind;

use crate::protocol::{Binding, BindingReport};

/// What the child is bound to, and whether a refusal from the daemon should be
/// explained as "no default profile".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Unbound,
    Bind(Binding),
    /// The child-kind default; a daemon refusal means it is not configured.
    Default(Binding),
}

impl Plan {
    pub fn binding(&self) -> Option<Binding> {
        match self {
            Plan::Unbound => None,
            Plan::Bind(b) | Plan::Default(b) => Some(b.clone()),
        }
    }
}

/// Decide a child's binding. `child_kind` is the env kind of the child's name
/// (`None` for pi and shells, which take no profile).
pub fn plan(
    parent: Option<&BindingReport>,
    child_kind: Option<&str>,
    profile: Option<&str>,
) -> Result<Plan, String> {
    let Some(kind) = child_kind else {
        return match profile {
            Some(_) => Err("--profile applies to claude, codex and dsh sessions; pi and shells take none".into()),
            None => Ok(Plan::Unbound),
        };
    };
    if let Some(id) = profile {
        if !env_kind::valid_set_name(id) {
            return Err(format!("{id:?} is not a profile id"));
        }
        return Ok(Plan::Bind(Binding { kind: kind.to_owned(), set: id.to_owned(), chain: None }));
    }
    let Some(parent) = parent.and_then(|p| p.binding.as_ref()) else {
        return Ok(Plan::Unbound);
    };
    if parent.kind == kind {
        return Ok(Plan::Bind(parent.clone()));
    }
    if parent.set == env_kind::NO_PROFILE {
        return Ok(Plan::Bind(Binding { kind: kind.to_owned(), set: env_kind::NO_PROFILE.into(), chain: None }));
    }
    Ok(Plan::Default(Binding { kind: kind.to_owned(), set: env_kind::DEFAULT_SET.into(), chain: None }))
}

/// The error a refused [`Plan::Default`] becomes.
pub fn no_default(parent_kind: &str, child_kind: &str) -> String {
    format!(
        "the parent session runs on a {parent_kind} profile and this host has no default {child_kind} \
         profile, so the child was not started on an unchosen provider — set a default {child_kind} \
         profile in rmux (Settings › Models) and reopen the parent, or pass --profile <id>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_claude_core::fallback::{ChainState, Exhaustion};

    fn report(kind: &str, set: &str, chain: Option<ChainState>) -> BindingReport {
        BindingReport {
            session: "claude-p".into(),
            binding: Some(Binding { kind: kind.into(), set: set.into(), chain }),
            ..Default::default()
        }
    }

    #[test]
    fn a_same_kind_child_inherits_the_set_and_the_chain_state() {
        let mut chain = ChainState { id: "c".into(), members: vec!["1".into(), "2".into()], active: "2".into(), ..Default::default() };
        chain.exhausted.insert("1".into(), Exhaustion { at: 10, resets_at: Some(99) });
        let parent = report("claude", "2", Some(chain.clone()));
        let Plan::Bind(b) = plan(Some(&parent), Some("claude"), None).unwrap() else { panic!() };
        assert_eq!(b.set, "2", "the child starts on the parent's *active* member");
        assert_eq!(b.chain, Some(chain), "and knows which members are already spent");
    }

    #[test]
    fn a_different_kind_uses_the_default_or_is_refused_with_a_reason() {
        let parent = report("claude", "7", None);
        let p = plan(Some(&parent), Some("openai"), None).unwrap();
        assert_eq!(p, Plan::Default(Binding { kind: "openai".into(), set: "default".into(), chain: None }));
        assert!(no_default("claude", "openai").contains("no default openai profile"));
        // A parent that chose no profile passes "no profile" on.
        let none = report("claude", "none", None);
        assert_eq!(
            plan(Some(&none), Some("deepseek"), None).unwrap().binding().unwrap().set,
            "none"
        );
    }

    #[test]
    fn an_explicit_profile_wins_and_unbound_parents_change_nothing() {
        let parent = report("claude", "7", None);
        assert_eq!(plan(Some(&parent), Some("claude"), Some("3")).unwrap().binding().unwrap().set, "3");
        assert_eq!(plan(None, Some("claude"), None).unwrap(), Plan::Unbound);
        assert_eq!(plan(Some(&parent), None, None).unwrap(), Plan::Unbound);
        assert!(plan(Some(&parent), None, Some("3")).is_err(), "pi takes no profile");
        assert!(plan(None, Some("claude"), Some("a b")).is_err());
    }
}
