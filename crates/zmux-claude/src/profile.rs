//! Model profiles — running Claude Code against something other than Anthropic.
//!
//! Claude Code is configured entirely through environment variables, so pointing
//! it at Kimi, GLM, an internal gateway or a reseller is a matter of setting the
//! right handful. People already have these as a block of `KEY=value \` lines
//! they paste into a shell, so that is exactly what this parses — retyping eight
//! variables into eight form fields is how a typo gets into a base URL.
//!
//! ## Two things make this different from an ordinary setting
//!
//! **A profile decides where the operator's credential is sent.** `ANTHROPIC_
//! BASE_URL` and `ANTHROPIC_AUTH_TOKEN` travel together: choosing a profile
//! points a bearer token at a host. So the destination host is something the UI
//! must state outright, never something to be inferred from a profile's name —
//! a profile called "GLM" can carry any URL at all.
//!
//! **Switching profiles has to unset, not just set.** The agent daemon merges
//! the environment it is handed, which is right for adding an account but wrong
//! here: moving from a profile with a base URL back to Anthropic proper would
//! otherwise leave the old URL in place, and the operator would be talking to
//! the previous provider while the UI said otherwise. So applying a profile
//! sends *every* variable zmux manages — the ones it wants set, and the rest
//! empty, which the daemon treats as a removal.
//!
//! Nothing here touches a keychain or a host. It is string handling, so it can
//! be tested without either.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Variables a profile is allowed to set, and which every apply clears.
///
/// The list is the contract: a variable missing from it would be settable but
/// never cleared, so it would survive a profile switch and quietly outlive the
/// profile that introduced it.
pub const MANAGED: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CODE_ATTRIBUTION_HEADER",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

/// Variables whose value is a credential.
///
/// These are redacted everywhere they are shown and are the reason a profile
/// lives in the keychain rather than in `localStorage` beside the session list.
pub const SECRET: &[&str] = &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"];

pub fn is_secret(key: &str) -> bool {
    SECRET.contains(&key)
}

/// A saved configuration the operator can pick by name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    /// Stable across renames, because sessions store it.
    pub id: String,
    pub name: String,
    pub vars: BTreeMap<String, String>,
}

impl ModelProfile {
    /// The provider host this profile sends requests — and the token — to.
    ///
    /// `None` means Anthropic, since that is what Claude Code does with no base
    /// URL set. Surfaced separately from the variables because it is the one
    /// fact about a profile that matters before choosing it.
    pub fn endpoint(&self) -> Option<&str> {
        self.vars.get("ANTHROPIC_BASE_URL").map(String::as_str).filter(|u| !u.is_empty())
    }

    /// Whether the profile carries a credential of its own.
    pub fn has_credential(&self) -> bool {
        SECRET.iter().any(|k| self.vars.get(*k).is_some_and(|v| !v.is_empty()))
    }

    /// The full set of variables to hand a daemon, including the clears.
    ///
    /// Every managed variable appears. The ones this profile does not set are
    /// empty, which is the daemon's signal to remove them — without that, the
    /// previous profile's base URL survives the switch and the session talks to
    /// a provider nobody selected. `extra` names variables other stored profiles
    /// use, so switching also clears anything a *different* profile introduced.
    pub fn apply_set<'a>(
        &self,
        extra: impl IntoIterator<Item = &'a str>,
    ) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = MANAGED
            .iter()
            .map(|k| ((*k).to_owned(), String::new()))
            .chain(extra.into_iter().map(|k| (k.to_owned(), String::new())))
            .collect();

        for (key, value) in &self.vars {
            if value.is_empty() {
                continue;
            }
            out.insert(key.clone(), value.clone());
        }
        out
    }
}

/// The set that clears every managed variable and sets nothing.
///
/// Selecting "Anthropic" is not the absence of a profile — it is an instruction
/// to undo whatever the last one did.
pub fn clearing_set<'a>(extra: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, String> {
    ModelProfile::default().apply_set(extra)
}

/// What a paste turned into, including what was ignored and why.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Parsed {
    pub vars: BTreeMap<String, String>,
    /// Keys that were dropped, named. Silently discarding half a paste and
    /// reporting success is how someone ends up debugging the wrong provider.
    pub ignored: Vec<String>,
    /// Things worth saying out loud before this is saved.
    pub warnings: Vec<String>,
}

/// Parse a pasted block of `KEY=value` assignments.
///
/// Accepts the shape people actually have: shell continuations (`\` at end of
/// line), a leading `export`, quoted values, comments, and the same pairs run
/// together on one line. Rejecting any of those would mean retyping, and
/// retyping a base URL is how a token gets pointed at the wrong host.
pub fn parse(input: &str) -> Parsed {
    let mut out = Parsed::default();

    for token in tokenize(input) {
        let token = token.strip_prefix("export ").unwrap_or(&token).trim().to_owned();
        if token.is_empty() || token.starts_with('#') {
            continue;
        }

        let Some((key, value)) = token.split_once('=') else {
            out.ignored.push(token);
            continue;
        };

        let key = key.trim();
        let value = unquote(value.trim());

        if !plausible_key(key) {
            out.ignored.push(key.to_owned());
            continue;
        }
        out.vars.insert(key.to_owned(), value);
    }

    out.warnings = warnings_for(&out.vars);
    out
}

/// Split a paste into `KEY=value` tokens.
///
/// Whitespace-separated, but quotes hold a value together — a custom-headers
/// variable legitimately contains spaces, and splitting it would silently
/// truncate a header at its first space.
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A shell line continuation is a separator, not content.
            '\\' if quote.is_none() => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if !current.trim().is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            '\'' | '"' if quote == Some(c) => {
                quote = None;
                current.push(c);
            }
            '\'' | '"' if quote.is_none() => {
                quote = Some(c);
                current.push(c);
            }
            c if c.is_whitespace() && quote.is_none() => {
                if !current.trim().is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Strip one matched pair of surrounding quotes.
fn unquote(value: &str) -> String {
    for q in ['\'', '"'] {
        if value.len() >= 2 && value.starts_with(q) && value.ends_with(q) {
            return value[1..value.len() - 1].to_owned();
        }
    }
    value.to_owned()
}

/// Whether a key is one zmux will carry.
///
/// Deliberately wider than [`MANAGED`]: Claude Code gains variables faster than
/// this list can, and refusing an unknown `ANTHROPIC_*` would mean the operator
/// cannot use a setting that exists. Anything outside the two namespaces is
/// refused, because a profile is not a general-purpose environment editor and
/// setting `PATH` or `LD_PRELOAD` from one is not a feature.
fn plausible_key(key: &str) -> bool {
    (key.starts_with("ANTHROPIC_") || key.starts_with("CLAUDE_CODE_"))
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Things the operator should be told before saving.
///
/// Warnings, not refusals: a plain-HTTP gateway on an internal network is a real
/// setup, and blocking it would make zmux useless to the person who has one.
/// What is not acceptable is *silence* — sending a bearer token in clear text
/// should be a decision, not an accident.
fn warnings_for(vars: &BTreeMap<String, String>) -> Vec<String> {
    let mut warnings = Vec::new();

    let base = vars.get("ANTHROPIC_BASE_URL").map(String::as_str).unwrap_or_default();
    let has_secret = SECRET.iter().any(|k| vars.get(*k).is_some_and(|v| !v.is_empty()));

    if !base.is_empty() && !base.starts_with("https://") && !base.starts_with("http://") {
        warnings.push(format!("{base} is not an http(s) URL — Claude will not be able to reach it"));
    } else if base.starts_with("http://") && has_secret && !is_loopback(base) {
        warnings.push(format!(
            "{base} is plain HTTP — the token would cross the network unencrypted"
        ));
    }

    if base.is_empty() && has_secret {
        warnings.push(
            "no base URL, so this token is sent to Anthropic's own API".to_owned(),
        );
    }

    if !base.is_empty() && !has_secret {
        warnings.push(
            "no token in this profile — the host's own Claude login will be used against this URL"
                .to_owned(),
        );
    }

    warnings
}

fn is_loopback(url: &str) -> bool {
    let rest = url.trim_start_matches("http://");
    rest.starts_with("localhost") || rest.starts_with("127.") || rest.starts_with("[::1]")
}

/// Show enough of a credential to recognise it, and no more.
pub fn redact(value: &str) -> String {
    let visible: String = value.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{visible}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape people paste — a shell block with trailing backslashes.
    const PASTED: &str = r"ANTHROPIC_BASE_URL=https://api.example.test/anthropic \
ANTHROPIC_AUTH_TOKEN=tok_abc123 \
ANTHROPIC_DEFAULT_OPUS_MODEL=vendor:large:text \
ANTHROPIC_DEFAULT_SONNET_MODEL=vendor:large:vision \
ANTHROPIC_DEFAULT_HAIKU_MODEL=vendor:small:text \
CLAUDE_CODE_SUBAGENT_MODEL=vendor:large:vision \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
CLAUDE_CODE_ATTRIBUTION_HEADER=0 \
";

    #[test]
    fn a_pasted_shell_block_parses_whole() {
        let parsed = parse(PASTED);
        assert_eq!(parsed.ignored, Vec::<String>::new(), "dropped part of the paste");
        assert_eq!(parsed.vars.len(), 8, "{:?}", parsed.vars);
        assert_eq!(
            parsed.vars.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://api.example.test/anthropic")
        );
        // The trailing backslash must not end up inside the last value.
        assert_eq!(parsed.vars.get("CLAUDE_CODE_ATTRIBUTION_HEADER").map(String::as_str), Some("0"));
        assert_eq!(parsed.vars.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str), Some("tok_abc123"));
    }

    #[test]
    fn the_same_pairs_on_one_line_parse_identically() {
        // Pasting out of a terminal loses the newlines. Same configuration, so
        // it must produce the same profile rather than one mangled variable.
        let one_line = PASTED.replace(" \\\n", " ");
        assert_eq!(parse(&one_line).vars, parse(PASTED).vars);
    }

    #[test]
    fn export_prefixes_and_quotes_come_off() {
        let parsed = parse("export ANTHROPIC_BASE_URL=\"https://x.test\"\nexport ANTHROPIC_MODEL='m'");
        assert_eq!(parsed.vars.get("ANTHROPIC_BASE_URL").map(String::as_str), Some("https://x.test"));
        assert_eq!(parsed.vars.get("ANTHROPIC_MODEL").map(String::as_str), Some("m"));
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        // Custom headers legitimately contain spaces; splitting on whitespace
        // would truncate the header at the first one and send half of it.
        let parsed = parse(r#"ANTHROPIC_CUSTOM_HEADERS="X-Team: platform eng""#);
        assert_eq!(
            parsed.vars.get("ANTHROPIC_CUSTOM_HEADERS").map(String::as_str),
            Some("X-Team: platform eng")
        );
    }

    #[test]
    fn a_profile_is_not_a_general_purpose_environment_editor() {
        // Setting PATH or LD_PRELOAD from a pasted block is not a feature, and
        // the keys are *named* rather than dropped in silence.
        let parsed = parse("PATH=/tmp/evil\nLD_PRELOAD=/tmp/x.so\nANTHROPIC_MODEL=ok");
        assert_eq!(parsed.vars.len(), 1);
        assert!(parsed.ignored.contains(&"PATH".to_owned()), "{:?}", parsed.ignored);
        assert!(parsed.ignored.contains(&"LD_PRELOAD".to_owned()), "{:?}", parsed.ignored);
    }

    #[test]
    fn an_unknown_anthropic_variable_is_still_carried() {
        // Claude Code gains variables faster than MANAGED can; refusing one the
        // operator needs is worse than carrying one zmux has not heard of.
        let parsed = parse("ANTHROPIC_SOMETHING_NEW=1");
        assert_eq!(parsed.vars.get("ANTHROPIC_SOMETHING_NEW").map(String::as_str), Some("1"));
        assert!(parsed.ignored.is_empty());
    }

    #[test]
    fn applying_a_profile_clears_what_the_last_one_set() {
        // The bug this prevents: switch from a custom endpoint back to
        // Anthropic and keep talking to the custom endpoint, because the daemon
        // merges rather than replaces.
        let profile = ModelProfile {
            id: "p".into(),
            name: "Vendor".into(),
            vars: parse(PASTED).vars,
        };

        let set = profile.apply_set(std::iter::empty());
        assert_eq!(
            set.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://api.example.test/anthropic")
        );
        // Managed but unset by this profile: present, and empty, which is the
        // daemon's removal signal. Absent would leave it set from before.
        assert_eq!(set.get("ANTHROPIC_API_KEY").map(String::as_str), Some(""));
        assert_eq!(set.get("CLAUDE_CODE_USE_BEDROCK").map(String::as_str), Some(""));

        // And going back to Anthropic clears the base URL rather than omitting it.
        let none = clearing_set(std::iter::empty());
        assert_eq!(none.get("ANTHROPIC_BASE_URL").map(String::as_str), Some(""));
        assert!(none.values().all(String::is_empty));
    }

    #[test]
    fn a_variable_only_another_profile_uses_is_cleared_too() {
        // Switching from a profile that set an exotic variable must not leave it
        // behind just because the incoming profile never heard of it.
        let profile = ModelProfile { id: "p".into(), name: "n".into(), ..Default::default() };
        let set = profile.apply_set(["ANTHROPIC_SOMETHING_NEW"]);
        assert_eq!(set.get("ANTHROPIC_SOMETHING_NEW").map(String::as_str), Some(""));
    }

    #[test]
    fn a_token_over_plain_http_is_called_out() {
        let parsed = parse("ANTHROPIC_BASE_URL=http://gateway.internal/v1\nANTHROPIC_AUTH_TOKEN=t");
        assert!(
            parsed.warnings.iter().any(|w| w.contains("plain HTTP")),
            "{:?}",
            parsed.warnings
        );

        // Loopback is not a network hop, so it must not cry wolf — a warning
        // shown for a safe case is a warning nobody reads on the unsafe one.
        let local = parse("ANTHROPIC_BASE_URL=http://localhost:8080\nANTHROPIC_AUTH_TOKEN=t");
        assert!(!local.warnings.iter().any(|w| w.contains("plain HTTP")), "{:?}", local.warnings);
    }

    #[test]
    fn a_token_with_no_base_url_says_where_it_is_going() {
        // The dangerous quiet case: a vendor token pasted without its URL is
        // sent to Anthropic, who will reject it — but the operator should know
        // the credential left for the wrong host.
        let parsed = parse("ANTHROPIC_AUTH_TOKEN=tok_x");
        assert!(parsed.warnings.iter().any(|w| w.contains("Anthropic")), "{:?}", parsed.warnings);
    }

    #[test]
    fn the_endpoint_is_read_from_the_profile_not_its_name() {
        let profile =
            ModelProfile { id: "1".into(), name: "GLM".into(), vars: parse(PASTED).vars };
        assert_eq!(profile.endpoint(), Some("https://api.example.test/anthropic"));
        assert!(profile.has_credential());
    }

    #[test]
    fn redaction_keeps_only_the_tail() {
        assert_eq!(redact("tok_abcdef1234"), "…1234");
        // Must not panic or reveal everything on a short value.
        assert_eq!(redact("ab"), "…ab");
        assert_eq!(redact(""), "…");
    }
}
