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
//! sends *every* variable rmux manages — the ones it wants set, and the rest
//! empty, which the daemon treats as a removal.
//!
//! Nothing here touches a keychain or a host. It is string handling, so it can
//! be tested without either.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which agent's credentials a profile carries.
///
/// A profile used to mean "Claude Code against some Anthropic-compatible
/// endpoint". It now also means DeepSeek Harness (`dsh`) against DeepSeek, or
/// Codex against OpenAI — three agents that read three disjoint sets of
/// variables. The kind decides which keys a paste may carry, which of them are
/// secrets, which one is the endpoint, and — on the host — **which sessions the
/// variables reach**: the daemon keeps one environment per kind and applies only
/// the matching one at spawn, so a DeepSeek key never reaches a Claude session
/// and an Anthropic base URL never reaches Codex.
///
/// `#[serde(default)]` on the field that carries it: every profile stored before
/// kinds existed is a Claude profile, and loading it as anything else would
/// repoint a saved session at a provider nobody chose.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    /// Claude Code (and anything Anthropic-compatible): `ANTHROPIC_*`, `CLAUDE_CODE_*`.
    #[default]
    Claude,
    /// DeepSeek Harness: `DEEPSEEK_*`.
    Deepseek,
    /// OpenAI Codex CLI: `OPENAI_*`, `CODEX_*`.
    Openai,
}

impl ProfileKind {
    pub const ALL: [ProfileKind; 3] = [ProfileKind::Claude, ProfileKind::Deepseek, ProfileKind::Openai];

    /// The name the agent daemon keys this kind's environment by
    /// (`rmux-agent setenv --kind <name>`). Also the serialised form.
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileKind::Claude => "claude",
            ProfileKind::Deepseek => "deepseek",
            ProfileKind::Openai => "openai",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        ProfileKind::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// Variables this kind manages, and which every apply of it clears.
    pub fn managed(self) -> &'static [&'static str] {
        match self {
            ProfileKind::Claude => MANAGED,
            ProfileKind::Deepseek => DEEPSEEK_MANAGED,
            ProfileKind::Openai => OPENAI_MANAGED,
        }
    }

    /// Variables of this kind whose value is a credential.
    pub fn secrets(self) -> &'static [&'static str] {
        match self {
            ProfileKind::Claude => SECRET,
            ProfileKind::Deepseek => &["DEEPSEEK_API_KEY"],
            ProfileKind::Openai => &["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"],
        }
    }

    /// The variable naming where requests — and the credential — are sent.
    pub fn endpoint_var(self) -> &'static str {
        match self {
            ProfileKind::Claude => "ANTHROPIC_BASE_URL",
            ProfileKind::Deepseek => "DEEPSEEK_BASE_URL",
            ProfileKind::Openai => "OPENAI_BASE_URL",
        }
    }

    /// The prefixes a key of this kind must start with.
    pub fn namespaces(self) -> &'static [&'static str] {
        match self {
            ProfileKind::Claude => &["ANTHROPIC_", "CLAUDE_CODE_"],
            ProfileKind::Deepseek => &["DEEPSEEK_"],
            ProfileKind::Openai => &["OPENAI_", "CODEX_"],
        }
    }

    /// Who the credential goes to when no endpoint is set.
    fn default_provider(self) -> &'static str {
        match self {
            ProfileKind::Claude => "Anthropic's own API",
            ProfileKind::Deepseek => "DeepSeek's own API (api.deepseek.com)",
            ProfileKind::Openai => "OpenAI's own API",
        }
    }

    /// The agent this kind is for, for messages.
    fn agent(self) -> &'static str {
        match self {
            ProfileKind::Claude => "Claude",
            ProfileKind::Deepseek => "dsh",
            ProfileKind::Openai => "Codex",
        }
    }
}

/// DeepSeek Harness's variables, from its own source
/// (`packages/llm/llm-deepseek-api-key`: `apiKeyEnv` defaults to
/// `DEEPSEEK_API_KEY`, and `$DEEPSEEK_BASE_URL` names a non-official endpoint).
pub const DEEPSEEK_MANAGED: &[&str] = &["DEEPSEEK_API_KEY", "DEEPSEEK_BASE_URL"];

/// Codex's variables. `OPENAI_API_KEY` and `CODEX_API_KEY` are read by Codex
/// itself. **`OPENAI_BASE_URL` and `OPENAI_MODEL` are read by rmux**, not Codex:
/// current Codex takes its base URL from configuration (`openai_base_url`) and
/// its model from `-m`, so rmux turns these two into `-c openai_base_url=…` and
/// `-m …` on the launch line — neither is a secret, so argv is the right place.
/// They are still delivered as environment too, which older Codex releases read.
pub const OPENAI_MANAGED: &[&str] = &[
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_BASE_URL",
    "OPENAI_MODEL",
    "OPENAI_ORGANIZATION",
    "OPENAI_PROJECT",
];

/// Variables a Claude profile is allowed to set, and which every apply clears.
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
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// Variables whose value is a credential.
///
/// These are redacted everywhere they are shown and are the reason a profile
/// lives in the keychain rather than in `localStorage` beside the session list.
pub const SECRET: &[&str] = &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];

/// What a subscription-token profile takes out of the session's environment:
/// every other way Claude Code could authenticate or be pointed elsewhere. With
/// any of them left in, the session would quietly run on the API key or the
/// gateway instead of the subscription the operator chose.
pub const TOKEN_CLEARS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL"];

/// What a profile that names its own endpoint takes out, unless it sets them
/// itself: the stored **Anthropic** account. A gateway profile must never carry
/// the operator's Anthropic credential to the gateway.
pub const ENDPOINT_CLEARS: &[&str] = &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];

/// Whether `key` holds a credential, for *any* kind. Used to redact, so it errs
/// towards hiding: a key that is a secret for one kind is hidden everywhere.
pub fn is_secret(key: &str) -> bool {
    ProfileKind::ALL.iter().any(|k| k.secrets().contains(&key))
}

/// A saved configuration the operator can pick by name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    /// Stable across renames, because sessions store it.
    pub id: String,
    pub name: String,
    /// Which agent this is for. Absent in every profile saved before kinds
    /// existed, all of which are Claude profiles — so the default is Claude.
    #[serde(default)]
    pub kind: ProfileKind,
    pub vars: BTreeMap<String, String>,
}

impl ModelProfile {
    /// The provider host this profile sends requests — and the token — to.
    ///
    /// `None` means Anthropic, since that is what Claude Code does with no base
    /// URL set. Surfaced separately from the variables because it is the one
    /// fact about a profile that matters before choosing it.
    pub fn endpoint(&self) -> Option<&str> {
        self.vars.get(self.kind.endpoint_var()).map(String::as_str).filter(|u| !u.is_empty())
    }

    /// Whether the profile carries a credential of its own.
    pub fn has_credential(&self) -> bool {
        self.kind.secrets().iter().any(|k| self.vars.get(*k).is_some_and(|v| !v.is_empty()))
    }

    /// A non-secret value this profile sets, if any.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str).filter(|v| !v.is_empty())
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
        let mut out: BTreeMap<String, String> = self
            .kind
            .managed()
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

impl ModelProfile {
    /// Whether this is a Claude subscription-token profile (`claude setup-token`).
    pub fn is_token_profile(&self) -> bool {
        self.kind == ProfileKind::Claude && self.value("CLAUDE_CODE_OAUTH_TOKEN").is_some()
    }

    /// The **named per-session set** this profile is delivered as: its own
    /// variables, plus removals (empty values) for whatever it must keep out of
    /// the environment the session would otherwise inherit.
    ///
    /// Unlike [`ModelProfile::apply_set`] this does not clear every managed
    /// variable — a named set replaces the per-kind set entirely, so there is no
    /// previous profile to undo. The removals are targeted, and they act on the
    /// *shared* environment (the stored Claude account) and the host's shell
    /// profile, because a session's set always wins over both:
    ///
    /// - a token profile removes [`TOKEN_CLEARS`];
    /// - a profile with its own endpoint removes [`ENDPOINT_CLEARS`] it does not set.
    pub fn session_set(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> =
            self.vars.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
        let clears: &[&str] = if self.is_token_profile() {
            TOKEN_CLEARS
        } else if self.kind == ProfileKind::Claude && self.endpoint().is_some() {
            ENDPOINT_CLEARS
        } else {
            &[]
        };
        for key in clears {
            out.entry((*key).to_owned()).or_default();
        }
        if self.is_token_profile() {
            for key in TOKEN_CLEARS {
                out.insert((*key).to_owned(), String::new());
            }
        }
        out
    }
}

/// A subscription token pasted for a new Claude account profile, checked.
///
/// Refuses, with the reason, an admin key (it administers the organisation and
/// must never reach a host), an API key (a different kind of credential, billed
/// differently — it belongs in an ordinary profile), and anything that is not a
/// token at all.
pub fn token_vars(token: &str) -> Result<BTreeMap<String, String>, String> {
    use crate::auth::CredentialKind;
    let token = token.trim();
    match CredentialKind::detect(token) {
        Some(CredentialKind::OauthToken) => {
            if token.chars().any(char::is_whitespace) {
                return Err("that token contains a space or line break — paste it as one line".into());
            }
            Ok([("CLAUDE_CODE_OAUTH_TOKEN".to_owned(), token.to_owned())].into_iter().collect())
        }
        Some(CredentialKind::AdminKey) => Err(
            "that is an organisation admin key (sk-ant-admin…). It administers the organisation and \
             cannot run Claude, so rmux never sends one to a server — paste the subscription token \
             from `claude setup-token` instead"
                .into(),
        ),
        Some(CredentialKind::ApiKey) => Err(
            "that is a Console API key (sk-ant-api…), not a subscription token — add it as an \
             ordinary Claude profile (ANTHROPIC_API_KEY=…) instead"
                .into(),
        ),
        None => Err("that does not look like a Claude subscription token (they start sk-ant-oat)".into()),
    }
}

/// A fallback chain: claude-kind profiles tried in order when one hits a
/// usage limit. Stored beside the profiles; a session's `modelProfile` may name
/// one by its id, which always starts `chain-` so it can never collide with a
/// profile id.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileChain {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: ProfileKind,
    pub members: Vec<String>,
    /// Switch on 529 / overloaded too. Off by default: overload is the
    /// provider being busy for everyone, and switching only spends the next
    /// account's window.
    #[serde(default)]
    pub switch_on_overloaded: bool,
}

/// The prefix every chain id carries.
pub const CHAIN_PREFIX: &str = "chain-";

pub fn is_chain_id(id: &str) -> bool {
    id.starts_with(CHAIN_PREFIX)
}

/// The whole keychain document: profiles, chains, and each kind's default.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStore {
    #[serde(default)]
    pub profiles: Vec<ModelProfile>,
    #[serde(default)]
    pub chains: Vec<ProfileChain>,
    /// Kind → profile id: what a child of a different kind binds to.
    #[serde(default)]
    pub defaults: BTreeMap<ProfileKind, String>,
}

impl ProfileStore {
    /// Read the document, accepting the shape every earlier build wrote — a
    /// bare array of profiles. Unreadable is empty, as before.
    pub fn parse(raw: &str) -> Self {
        if let Ok(store) = serde_json::from_str::<ProfileStore>(raw) {
            return store;
        }
        match serde_json::from_str::<Vec<ModelProfile>>(raw) {
            Ok(profiles) => ProfileStore { profiles, ..Default::default() },
            Err(_) => ProfileStore::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty() && self.chains.is_empty() && self.defaults.is_empty()
    }

    pub fn profile(&self, id: &str) -> Option<&ModelProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn chain(&self, id: &str) -> Option<&ProfileChain> {
        self.chains.iter().find(|c| c.id == id)
    }

    /// Remove a profile everywhere it is named: chains lose the member, a
    /// default naming it is cleared. A chain left empty is removed too.
    pub fn remove_profile(&mut self, id: &str) {
        self.profiles.retain(|p| p.id != id);
        for chain in &mut self.chains {
            chain.members.retain(|m| m != id);
        }
        self.chains.retain(|c| !c.members.is_empty());
        self.defaults.retain(|_, v| v != id);
    }
}

/// The set that clears every managed variable and sets nothing.
///
/// Selecting "Anthropic" is not the absence of a profile — it is an instruction
/// to undo whatever the last one did.
pub fn clearing_set<'a>(extra: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, String> {
    clearing_set_for(ProfileKind::Claude, extra)
}

/// [`clearing_set`] for a given kind.
pub fn clearing_set_for<'a>(
    kind: ProfileKind,
    extra: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    ModelProfile { kind, ..Default::default() }.apply_set(extra)
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
    parse_for(ProfileKind::Claude, input)
}

/// [`parse`] for a given kind: only that kind's namespaces are carried, and a
/// key from another kind's namespace is *named* among the ignored — pasting an
/// `ANTHROPIC_API_KEY` into a Codex profile is a mistake worth saying out loud.
pub fn parse_for(kind: ProfileKind, input: &str) -> Parsed {
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

        if !plausible_key(kind, key) {
            out.ignored.push(key.to_owned());
            continue;
        }
        out.vars.insert(key.to_owned(), value);
    }

    out.warnings = warnings_for(kind, &out.vars);
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

/// Whether a key is one rmux will carry.
///
/// Deliberately wider than [`MANAGED`]: Claude Code gains variables faster than
/// this list can, and refusing an unknown `ANTHROPIC_*` would mean the operator
/// cannot use a setting that exists. Anything outside the two namespaces is
/// refused, because a profile is not a general-purpose environment editor and
/// setting `PATH` or `LD_PRELOAD` from one is not a feature.
fn plausible_key(kind: ProfileKind, key: &str) -> bool {
    kind.namespaces().iter().any(|ns| key.starts_with(ns))
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Things the operator should be told before saving.
///
/// Warnings, not refusals: a plain-HTTP gateway on an internal network is a real
/// setup, and blocking it would make rmux useless to the person who has one.
/// What is not acceptable is *silence* — sending a bearer token in clear text
/// should be a decision, not an accident.
fn warnings_for(kind: ProfileKind, vars: &BTreeMap<String, String>) -> Vec<String> {
    let mut warnings = Vec::new();

    let base = vars.get(kind.endpoint_var()).map(String::as_str).unwrap_or_default();
    let has_secret = kind.secrets().iter().any(|k| vars.get(*k).is_some_and(|v| !v.is_empty()));
    let agent = kind.agent();

    if !base.is_empty() && !base.starts_with("https://") && !base.starts_with("http://") {
        warnings.push(format!("{base} is not an http(s) URL — {agent} will not be able to reach it"));
    } else if base.starts_with("http://") && has_secret && !is_loopback(base) {
        warnings.push(format!(
            "{base} is plain HTTP — the token would cross the network unencrypted"
        ));
    }

    if base.is_empty() && has_secret {
        warnings.push(format!("no base URL, so this token is sent to {}", kind.default_provider()));
    }

    if !base.is_empty() && !has_secret {
        warnings.push(format!(
            "no token in this profile — the host's own {agent} login will be used against this URL"
        ));
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
        // operator needs is worse than carrying one rmux has not heard of.
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
            ..Default::default()
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
            ModelProfile { id: "1".into(), name: "GLM".into(), vars: parse(PASTED).vars, ..Default::default() };
        assert_eq!(profile.endpoint(), Some("https://api.example.test/anthropic"));
        assert!(profile.has_credential());
    }

    #[test]
    fn a_profile_saved_before_kinds_existed_loads_as_claude() {
        // The shape every stored profile had until now: no `kind`. Loading it as
        // anything but Claude would repoint saved sessions at another provider.
        let old = r#"[{"id":"1","name":"GLM","vars":{"ANTHROPIC_BASE_URL":"https://x.test"}}]"#;
        let loaded: Vec<ModelProfile> = serde_json::from_str(old).unwrap();
        assert_eq!(loaded[0].kind, ProfileKind::Claude);
        assert_eq!(loaded[0].endpoint(), Some("https://x.test"));
        // And it round-trips with the kind now explicit.
        let again = serde_json::to_string(&loaded).unwrap();
        assert!(again.contains(r#""kind":"claude""#), "{again}");
    }

    #[test]
    fn each_kind_carries_only_its_own_namespace_and_names_the_rest() {
        let paste = "DEEPSEEK_API_KEY=sk-ds\nDEEPSEEK_BASE_URL=https://api.deepseek.com\n\
                     ANTHROPIC_API_KEY=sk-ant\nOPENAI_API_KEY=sk-oai\nPATH=/tmp/evil\nLD_PRELOAD=/x.so";
        let ds = parse_for(ProfileKind::Deepseek, paste);
        assert_eq!(ds.vars.keys().collect::<Vec<_>>(), ["DEEPSEEK_API_KEY", "DEEPSEEK_BASE_URL"]);
        for refused in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "PATH", "LD_PRELOAD"] {
            assert!(ds.ignored.contains(&refused.to_owned()), "{refused} not named: {:?}", ds.ignored);
        }

        let oai = parse_for(ProfileKind::Openai, "OPENAI_API_KEY=k\nCODEX_API_KEY=c\nOPENAI_MODEL=gpt-5\nDEEPSEEK_API_KEY=d");
        assert_eq!(oai.vars.len(), 3, "{:?}", oai.vars);
        assert!(oai.ignored.contains(&"DEEPSEEK_API_KEY".to_owned()));

        // Claude's parse is unchanged by all of this.
        let claude = parse("OPENAI_API_KEY=k\nANTHROPIC_MODEL=m");
        assert_eq!(claude.vars.len(), 1);
        assert!(claude.ignored.contains(&"OPENAI_API_KEY".to_owned()));
    }

    #[test]
    fn the_endpoint_and_the_secrets_follow_the_kind() {
        let ds = ModelProfile {
            id: "2".into(),
            name: "DeepSeek".into(),
            kind: ProfileKind::Deepseek,
            vars: parse_for(ProfileKind::Deepseek, "DEEPSEEK_API_KEY=sk\nDEEPSEEK_BASE_URL=https://ds.test").vars,
        };
        assert_eq!(ds.endpoint(), Some("https://ds.test"));
        assert!(ds.has_credential());
        assert!(is_secret("DEEPSEEK_API_KEY") && is_secret("OPENAI_API_KEY") && is_secret("CODEX_API_KEY"));
        assert!(!is_secret("OPENAI_MODEL"));

        // Applying clears this kind's managed set and nothing of another kind's.
        let set = ds.apply_set(std::iter::empty());
        assert_eq!(set.get("DEEPSEEK_API_KEY").map(String::as_str), Some("sk"));
        assert!(!set.contains_key("ANTHROPIC_BASE_URL"), "{set:?}");
        let none = clearing_set_for(ProfileKind::Openai, std::iter::empty());
        assert_eq!(none.get("OPENAI_API_KEY").map(String::as_str), Some(""));
        assert!(none.keys().all(|k| k.starts_with("OPENAI_") || k.starts_with("CODEX_")), "{none:?}");
    }

    #[test]
    fn profile_kinds_are_exactly_the_kinds_the_daemon_holds() {
        // Two names for one thing, on two sides of a wire. A kind the daemon does
        // not know is refused there, so a drift here would fail every session of
        // that kind — loudly, but still broken.
        let ours: Vec<&str> = ProfileKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(ours, rmux_claude_core::env_kind::KINDS);
        for k in ProfileKind::ALL {
            assert_eq!(ProfileKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{}\"", k.as_str()));
        }
    }

    #[test]
    fn a_deepseek_key_without_a_base_url_says_where_it_goes() {
        let parsed = parse_for(ProfileKind::Deepseek, "DEEPSEEK_API_KEY=sk");
        assert!(parsed.warnings.iter().any(|w| w.contains("DeepSeek")), "{:?}", parsed.warnings);
    }

    #[test]
    fn a_token_profile_sets_the_token_and_clears_every_other_route() {
        let vars = token_vars("  sk-ant-oat01-abcdef \n").unwrap();
        let p = ModelProfile { id: "1".into(), name: "Max".into(), kind: ProfileKind::Claude, vars };
        assert!(p.is_token_profile());
        let set = p.session_set();
        assert_eq!(set.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str), Some("sk-ant-oat01-abcdef"));
        for key in TOKEN_CLEARS {
            assert_eq!(set.get(*key).map(String::as_str), Some(""), "{key} must be cleared");
        }
        assert!(is_secret("CLAUDE_CODE_OAUTH_TOKEN"), "the token must be redacted in the UI");
    }

    #[test]
    fn an_admin_key_or_an_api_key_is_refused_as_a_token_with_the_reason() {
        let admin = token_vars("sk-ant-admin01-xyz").unwrap_err();
        assert!(admin.contains("admin key") && admin.contains("never"), "{admin}");
        let api = token_vars("sk-ant-api03-xyz").unwrap_err();
        assert!(api.contains("API key"), "{api}");
        assert!(token_vars("hunter2").is_err());
    }

    #[test]
    fn a_gateway_profile_keeps_the_anthropic_account_away_from_the_gateway() {
        let p = ModelProfile {
            id: "2".into(),
            name: "GLM".into(),
            kind: ProfileKind::Claude,
            vars: parse("ANTHROPIC_BASE_URL=https://glm.test ANTHROPIC_AUTH_TOKEN=t").vars,
        };
        let set = p.session_set();
        assert_eq!(set.get("ANTHROPIC_API_KEY").map(String::as_str), Some(""));
        assert_eq!(set.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str), Some(""));
        assert_eq!(set.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str), Some("t"));
        // A plain model-override profile removes nothing.
        let m = ModelProfile { kind: ProfileKind::Claude, vars: parse("ANTHROPIC_MODEL=opus").vars, ..Default::default() };
        assert!(m.session_set().values().all(|v| !v.is_empty()));
    }

    #[test]
    fn the_old_array_document_loads_and_chains_migrate_safely() {
        let old = r#"[{"id":"1","name":"GLM","vars":{"ANTHROPIC_BASE_URL":"https://glm.test"}}]"#;
        let store = ProfileStore::parse(old);
        assert_eq!(store.profiles.len(), 1);
        assert_eq!(store.profiles[0].kind, ProfileKind::Claude);
        assert!(store.chains.is_empty());
        let mut store = ProfileStore {
            chains: vec![ProfileChain { id: "chain-1".into(), name: "c".into(), members: vec!["1".into(), "2".into()], ..Default::default() }],
            ..store
        };
        store.defaults.insert(ProfileKind::Claude, "1".into());
        let raw = serde_json::to_string(&store).unwrap();
        assert_eq!(ProfileStore::parse(&raw), store, "the new document round-trips");
        store.remove_profile("1");
        assert_eq!(store.chains[0].members, ["2"]);
        assert!(store.defaults.is_empty());
        store.remove_profile("2");
        assert!(store.chains.is_empty(), "an empty chain is removed with its last member");
        assert!(is_chain_id("chain-3") && !is_chain_id("3"));
        assert_eq!(ProfileStore::parse("garbage"), ProfileStore::default());
    }

    #[test]
    fn redaction_keeps_only_the_tail() {
        assert_eq!(redact("tok_abcdef1234"), "…1234");
        // Must not panic or reveal everything on a short value.
        assert_eq!(redact("ab"), "…ab");
        assert_eq!(redact(""), "…");
    }
}
