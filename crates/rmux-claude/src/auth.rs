//! Signing in to Claude once, and using that account anywhere.
//!
//! `claude setup-token` runs an OAuth flow and prints a **long-lived token** —
//! the mechanism Claude Code provides for machines where a browser login is not
//! practical, which is every remote host rmux talks to.
//!
//! rmux runs it in a PTY, spots the URL it prints, opens that in the *local*
//! browser, and captures the token from the output. The token is then kept in the
//! OS keychain and handed to sessions as `CLAUDE_CODE_OAUTH_TOKEN`, so one login
//! covers every host instead of one login per host.
//!
//! **The token never becomes a command-line argument.** `ps` shows one user's
//! command line to every account on a machine, so a token passed as a flag is
//! disclosed host-wide. It travels over the agent's `0600` socket instead — see
//! `rmux_agent::protocol::Frame::SetEnv` — and the daemon keeps it in memory only.

/// The environment variable Claude Code reads a long-lived OAuth token from.
pub const TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// The variable Claude Code reads a Console API key from.
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// What kind of credential rmux was handed.
///
/// The three look alike and are used completely differently, so they are told
/// apart by prefix rather than by asking the operator to classify their own
/// secret — getting it wrong means an authentication failure with no useful
/// message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    /// `sk-ant-oat…` from `claude setup-token`. Authenticates a **subscription**
    /// and can only make model requests — notably it cannot read usage.
    OauthToken,
    /// `sk-ant-api…` from the Console. Organisation-billed, which is what makes
    /// it the right thing to share with a team; a subscription login is not.
    ApiKey,
    /// `sk-ant-admin…`. Reads the organisation's usage report and **must never**
    /// be handed to a host: it administers the org, it does not run models.
    AdminKey,
}

impl CredentialKind {
    /// Classify a credential by its prefix.
    pub fn detect(credential: &str) -> Option<Self> {
        let credential = credential.trim();
        // Admin first: `sk-ant-admin…` would also match a looser API-key test,
        // and confusing the two would send an org-administration key to a
        // remote host.
        if credential.starts_with("sk-ant-admin") {
            return Some(Self::AdminKey);
        }
        if credential.starts_with("sk-ant-oat") {
            return Some(Self::OauthToken);
        }
        if credential.starts_with("sk-ant-api") {
            return Some(Self::ApiKey);
        }
        None
    }

    /// The environment variable that carries this credential to Claude Code.
    ///
    /// `None` for an admin key: it is for the usage API only, and putting it on
    /// a host would hand org administration to every session running there.
    pub fn env_var(self) -> Option<&'static str> {
        match self {
            Self::OauthToken => Some(TOKEN_ENV),
            Self::ApiKey => Some(API_KEY_ENV),
            Self::AdminKey => None,
        }
    }

    /// Whether this credential may be sent to a target host at all.
    pub fn may_leave_this_machine(self) -> bool {
        self.env_var().is_some()
    }
}

/// The command that starts the flow.
pub fn setup_token_command() -> String {
    "claude setup-token".to_owned()
}

/// Find the authorisation URL in `setup-token`'s output.
///
/// Matched by prefix rather than by parsing the surrounding prose: the wording
/// changes between releases, and the URL is the only part that has to be exact.
/// Trailing punctuation and the escape sequences a TUI wraps around it are
/// stripped, because the string is going to a browser.
pub fn find_auth_url(output: &str) -> Option<String> {
    // Several hosts, because the one the CLI prints has moved. A real run of
    // `claude setup-token` (v2.1.22) emits `https://claude.com/cai/oauth/…`;
    // older builds used `claude.ai`, and the Console flow uses its own host.
    // Matching only the historical two meant the URL was never found and the
    // login hung until it timed out — with the link sitting in the buffer.
    const PREFIXES: [&str; 4] = [
        "https://claude.com/cai/oauth",
        "https://claude.com/oauth",
        "https://claude.ai/oauth",
        "https://console.anthropic.com/oauth",
    ];

    for prefix in PREFIXES {
        let Some(at) = output.find(prefix) else { continue };
        let rest = &output[at..];

        // Ends at the first character that cannot be in a URL. Escape sequences
        // and line wrapping both terminate here.
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '\u{1b}' || c == '"' || c == '\'')
            .unwrap_or(rest.len());

        let url = rest[..end].trim_end_matches(['.', ',', ')', ']']);
        if url.len() > prefix.len() {
            return Some(url.to_owned());
        }
    }
    None
}

/// Pull the token out of `setup-token`'s output.
///
/// Claude prints tokens with a `sk-ant-oat` prefix. Anchoring on that rather than
/// on "the last line" survives the surrounding text changing, and — more
/// importantly — cannot mistake a prompt or a wrapped banner for a credential.
pub fn find_token(output: &str) -> Option<String> {
    const PREFIX: &str = "sk-ant-oat";

    let at = output.find(PREFIX)?;
    let rest = &output[at..];

    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());

    let token = &rest[..end];
    // A bare prefix is a banner, not a credential.
    (token.len() > PREFIX.len() + 8).then(|| token.to_owned())
}

/// Redact a token for display and logs.
///
/// Enough to recognise which account is in use, never enough to use it. Anything
/// that prints a token in full eventually prints it somewhere it should not be.
pub fn redact(token: &str) -> String {
    let tail: String = token.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_url_is_found_amid_tui_decoration() {
        // What this actually arrives as: wrapped in colour codes, mid-sentence.
        let output = "\u{1b}[36mOpen this URL:\u{1b}[39m \
                      https://claude.ai/oauth/authorize?code=1&state=abc\u{1b}[0m\r\n";
        assert_eq!(
            find_auth_url(output).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=1&state=abc")
        );
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_url() {
        // "visit https://claude.ai/oauth/x." — the full stop is prose.
        let url = find_auth_url("visit https://claude.ai/oauth/authorize?x=1.").unwrap();
        assert!(url.ends_with("x=1"), "{url}");
    }

    #[test]
    fn the_url_a_real_setup_token_run_prints_is_recognised() {
        // Captured verbatim from `claude setup-token` v2.1.22. This is the exact
        // string the login depends on; when it was not in the prefix list the
        // flow simply hung, which is why it is pinned rather than described.
        let output = "Browser didn't open? Use the url below to sign in (c to copy)\r\n\r\n\
            https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-\
            5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth\
            %2Fcode%2Fcallback&scope=user%3Ainference&code_challenge_method=S256\r\n";

        let url = find_auth_url(output).expect("the sign-in link must be found");
        assert!(url.starts_with("https://claude.com/cai/oauth/authorize"), "{url}");
        // The whole query has to survive: PKCE breaks without code_challenge.
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert!(url.contains("client_id=9d1c250a"), "{url}");
        // …and stop at the newline rather than swallowing the rest of the TUI.
        assert!(!url.contains('\r') && !url.contains('\n'), "{url}");
    }

    #[test]
    fn every_host_the_cli_has_used_is_still_recognised() {
        for url in [
            "https://claude.com/cai/oauth/authorize?x=1",
            "https://claude.com/oauth/authorize?x=1",
            "https://claude.ai/oauth/authorize?x=1",
            "https://console.anthropic.com/oauth/authorize?x=1",
        ] {
            assert_eq!(find_auth_url(&format!("go to {url} now")).as_deref(), Some(url));
        }
    }

    #[test]
    fn no_url_is_none_rather_than_a_fragment() {
        assert!(find_auth_url("Signing in…").is_none());
        // A bare prefix with nothing after it is not a usable URL.
        assert!(find_auth_url("https://claude.ai/oauth").is_none());
    }

    #[test]
    fn the_token_is_extracted_without_its_surroundings() {
        let output = "Success!\r\n\r\n  sk-ant-oat01-AbCd1234_efGH-5678  \r\n\r\nCopy this.";
        assert_eq!(find_token(output).as_deref(), Some("sk-ant-oat01-AbCd1234_efGH-5678"));
    }

    #[test]
    fn a_banner_is_not_mistaken_for_a_token() {
        // The word appearing in prose must not be captured as a credential.
        assert!(find_token("your sk-ant-oat token will appear below").is_none());
        assert!(find_token("no token here").is_none());
    }

    #[test]
    fn credentials_are_told_apart_by_prefix() {
        assert_eq!(CredentialKind::detect("sk-ant-oat01-abc"), Some(CredentialKind::OauthToken));
        assert_eq!(CredentialKind::detect("sk-ant-api03-abc"), Some(CredentialKind::ApiKey));
        assert_eq!(CredentialKind::detect("sk-ant-admin01-abc"), Some(CredentialKind::AdminKey));
        assert_eq!(CredentialKind::detect("hunter2"), None);
        // Whitespace from a paste must not change the classification.
        assert_eq!(CredentialKind::detect("  sk-ant-api03-x \n"), Some(CredentialKind::ApiKey));
    }

    #[test]
    fn an_admin_key_is_never_classified_as_an_api_key() {
        // Both begin `sk-ant-a`. Ordering the checks wrong would send a key that
        // administers the organisation to every host the operator codes on.
        let kind = CredentialKind::detect("sk-ant-admin01-abc").unwrap();
        assert_eq!(kind, CredentialKind::AdminKey);
        assert!(!kind.may_leave_this_machine());
        assert_eq!(kind.env_var(), None);
    }

    #[test]
    fn each_runnable_credential_has_the_variable_claude_reads() {
        assert_eq!(CredentialKind::OauthToken.env_var(), Some("CLAUDE_CODE_OAUTH_TOKEN"));
        assert_eq!(CredentialKind::ApiKey.env_var(), Some("ANTHROPIC_API_KEY"));
        assert!(CredentialKind::ApiKey.may_leave_this_machine());
    }

    #[test]
    fn redaction_keeps_only_enough_to_recognise_it() {
        let redacted = redact("sk-ant-oat01-SECRETSECRET-tail");
        assert_eq!(redacted, "…tail");
        assert!(!redacted.contains("SECRET"), "{redacted}");
    }

    #[test]
    fn redaction_is_safe_for_a_short_string() {
        // Never panic on unexpected input — this runs on a value we did not
        // produce, and panicking here would take a session down.
        assert_eq!(redact(""), "…");
        assert_eq!(redact("ab"), "…ab");
    }
}
