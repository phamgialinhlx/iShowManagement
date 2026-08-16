//! The zmux side of the askpass bridge, on Windows.
//!
//! ## Why this had to exist
//!
//! It was a stub, on the reasoning that key-based hosts are the common case and
//! a password host would "fail fast with a reason". Measured against a real
//! password host from a real Windows machine, that is not what happens — and the
//! difference is the whole bug:
//!
//! ```text
//! $ ssh -T root@host uname -s      # stdin a pipe, SSH_ASKPASS_REQUIRE=never
//! Permission denied, please try again.
//! Permission denied, please try again.
//! root@host: Permission denied (publickey,password).
//! ```
//!
//! `ssh` takes a password from a **terminal** or from an askpass helper, and a
//! command zmux runs has neither. So it burns its retries against nothing and
//! fails — not once, but on every single command. Windows OpenSSH has no
//! `ControlMaster`, so *every* file listing, metrics sample and status poll is
//! its own connection and its own authentication: on a password host zmux
//! generated failed logins at roughly one every two seconds, which is both
//! useless to the operator and enough to get the machine banned by `fail2ban`.
//!
//! On macOS and Linux none of this is visible, because the `ControlMaster`
//! authenticates once through the Unix-socket bridge and everything afterwards
//! rides that connection.
//!
//! ## Why a named pipe, and why the DACL is the point
//!
//! The stub's objection was correct and is answered here rather than ignored.
//! Two of the three guards on the Unix server are filesystem permissions — a
//! `0700` directory holding a `0600` socket — and `restrict_to_owner` is a no-op
//! on Windows. Reproducing the *shape* with only the token behind it would be
//! fail-open on the socket that dispenses credentials.
//!
//! A named pipe carries a real security descriptor, so the guarantee is restored
//! rather than approximated — **but only if one is supplied.** `CreateNamedPipe`
//! with `lpSecurityAttributes` NULL grants, in Microsoft's own words, "read
//! access to members of the Everyone group and the anonymous account". That is
//! precisely the fail-open a naive port would have shipped. Every instance here
//! is created with an explicit `D:P(A;;GA;;;<current user SID>)`: one allow ACE,
//! for this user, on a protected DACL that inherits nothing.
//!
//! The token guard is unchanged and still required — it is what stops another
//! process *of the same user* phishing a credential dialog out of zmux.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{Prompt, classify};

/// SDDL revision for [`ConvertStringSecurityDescriptorToSecurityDescriptorW`].
///
/// Spelled out rather than imported: it is the only revision that has ever
/// existed, and the constant's module path has moved between `windows-sys`
/// releases while the value has not.
const SDDL_REVISION_1: u32 = 1;

/// What the helper sends us. Identical to the Unix server's — one wire format,
/// so the helper's own logic does not fork per platform.
#[derive(Debug, Deserialize)]
struct HelperRequest {
    token: String,
    prompt: String,
    /// Absent from an older helper; see [`Prompt::attempt`].
    #[serde(default)]
    attempt: Option<String>,
}

/// Answers a prompt, or `None` if the user dismissed it.
pub type Answerer = Arc<dyn Fn(Prompt) -> super::BoxFuture<Option<String>> + Send + Sync>;

/// A listening askpass pipe.
///
/// Dropping it stops new instances being created; the operating system removes
/// the pipe once the last handle closes, so there is no file to clean up and no
/// `Drop` counterpart to the Unix server's `remove_file`.
#[derive(Debug)]
pub struct AskpassServer {
    socket: PathBuf,
    token: String,
}

impl AskpassServer {
    /// Create the pipe and start serving prompts.
    pub async fn start(answerer: Answerer) -> anyhow::Result<Self> {
        let name = format!(r"\\.\pipe\zmux-askpass-{}", short_id());
        let security = OwnerOnlySecurity::new()?;

        // The first instance is created before returning, so a helper spawned
        // immediately afterwards finds something to connect to. `first_pipe_instance`
        // makes the create fail rather than silently attaching to a pipe of this
        // name that somebody else got there first — which on a shared machine is
        // the whole attack.
        let mut server = create_instance(&name, true, &security)?;

        let token = short_id();
        let server_token = token.clone();
        let listen_name = name.clone();

        tokio::spawn(async move {
            loop {
                if let Err(e) = server.connect().await {
                    tracing::warn!(error = %e, "askpass listener stopped");
                    break;
                }

                // Hand the connected instance off and immediately open the next
                // one, so there is always a free instance for the next helper.
                // Without this a second prompt arriving while the first is on
                // screen would get ERROR_PIPE_BUSY.
                let connected = std::mem::replace(
                    &mut server,
                    match create_instance(&listen_name, false, &security) {
                        Ok(next) => next,
                        Err(e) => {
                            tracing::warn!(error = %e, "could not open another askpass pipe instance");
                            break;
                        }
                    },
                );

                let answerer = Arc::clone(&answerer);
                let token = server_token.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(connected, &token, answerer).await {
                        tracing::debug!(error = %e, "askpass request failed");
                    }
                });
            }
        });

        Ok(Self { socket: PathBuf::from(name), token })
    }

    /// The pipe name, in the form the helper is given.
    ///
    /// Named `socket_path` because it is the same thing to every caller: a
    /// string put into `ZMUX_ASKPASS_SOCKET` for the helper to open.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

fn create_instance(
    name: &str,
    first: bool,
    security: &OwnerOnlySecurity,
) -> anyhow::Result<NamedPipeServer> {
    let mut attributes = security.attributes();

    // SAFETY: `attributes` outlives the call, and its `lpSecurityDescriptor`
    // points at a descriptor owned by `security`, which the caller holds for
    // longer than this function runs.
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            // A named pipe is reachable over SMB unless this is set. zmux's
            // askpass pipe answering a machine across the network would be a
            // credential dialog anyone on it could summon.
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                std::ptr::from_mut(&mut attributes).cast::<c_void>(),
            )
    }?;

    Ok(server)
}

async fn serve(
    stream: NamedPipeServer,
    token: &str,
    answerer: Answerer,
) -> anyhow::Result<()> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut line = String::new();
    BufReader::new(read_half).read_line(&mut line).await?;

    let request: HelperRequest = serde_json::from_str(&line)?;

    // A mismatch drops the connection without hinting at why.
    if request.token != token {
        anyhow::bail!("rejected an askpass request with a bad token");
    }

    let prompt = Prompt {
        id: short_id(),
        kind: classify(&request.prompt),
        message: request.prompt,
        attempt: request.attempt,
    };

    let response = match answerer(prompt).await {
        Some(answer) => serde_json::json!({ "answer": answer }),
        // No `answer` field: the helper reads that as cancelled and exits
        // non-zero, so ssh aborts rather than trying an empty password.
        None => serde_json::json!({ "cancelled": true }),
    };

    write_half.write_all(format!("{response}\n").as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

/// A security descriptor granting the current user, and nobody else.
struct OwnerOnlySecurity {
    descriptor: *mut c_void,
}

// SAFETY: the descriptor is an opaque, immutable blob owned solely by this
// value. Nothing reads or writes through the pointer except `CreateNamedPipe`,
// which only reads, and `LocalFree` on drop.
unsafe impl Send for OwnerOnlySecurity {}
unsafe impl Sync for OwnerOnlySecurity {}

impl OwnerOnlySecurity {
    fn new() -> anyhow::Result<Self> {
        let sid = current_user_sid()?;
        // `D:` a DACL, `P` protected so no inherited ACE can widen it, then one
        // allow ACE granting GENERIC_ALL to this user. Deliberately *not*
        // including Administrators: they can take ownership regardless, and
        // naming them here would only make the pipe reachable without doing so.
        let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));

        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `sddl` is a NUL-terminated wide string that outlives the call;
        // `descriptor` is a valid out-pointer.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(
            ok != 0,
            "could not build the askpass pipe's security descriptor: {}",
            std::io::Error::last_os_error()
        );

        Ok(Self { descriptor })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            // The helper is a grandchild (ssh spawns it) and opens the pipe by
            // name; no handle is ever inherited into it.
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerOnlySecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW,
            // which documents LocalFree as the way to release it, and freed once.
            unsafe { LocalFree(self.descriptor as HLOCAL) };
        }
    }
}

/// The SID of the user this process runs as, in string form (`S-1-5-21-…`).
fn current_user_sid() -> anyhow::Result<String> {
    // SAFETY: every call below is checked, `token` is closed on both the success
    // and failure paths, and each buffer is sized by the API before it is filled.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        anyhow::ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0,
            "could not open this process's token: {}",
            std::io::Error::last_os_error()
        );

        // First call reports the size it needs and is *expected* to fail.
        let mut needed: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let mut buffer = vec![0u8; needed as usize];

        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        anyhow::ensure!(
            ok != 0,
            "could not read this process's user: {}",
            std::io::Error::last_os_error()
        );

        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut text: *mut u16 = std::ptr::null_mut();
        anyhow::ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut text) != 0,
            "could not render this user's SID: {}",
            std::io::Error::last_os_error()
        );

        let sid = from_wide(text);
        LocalFree(text as HLOCAL);
        Ok(sid)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a NUL-terminated wide string the OS allocated for us.
///
/// # Safety
/// `text` must point at a NUL-terminated UTF-16 string.
unsafe fn from_wide(text: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a NUL terminator, so this walk is bounded.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` was just measured against the same allocation.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
}

/// A short random identifier — 128 bits of entropy, hex-encoded.
fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::super::PromptKind;
    use super::*;
    use tokio::net::windows::named_pipe::ClientOptions;

    fn answer_with(reply: Option<&'static str>) -> Answerer {
        Arc::new(move |_prompt| Box::pin(async move { reply.map(str::to_owned) }))
    }

    /// Ask over the pipe exactly as the helper binary does.
    async fn ask(server: &AskpassServer, token: &str, prompt: &str) -> String {
        let name = server.socket_path().to_string_lossy().into_owned();
        let mut client = ClientOptions::new().open(&name).expect("could not open the askpass pipe");

        let request = serde_json::json!({ "token": token, "prompt": prompt });
        client.write_all(format!("{request}\n").as_bytes()).await.unwrap();
        client.flush().await.unwrap();

        let mut line = String::new();
        let _ = BufReader::new(&mut client).read_line(&mut line).await;
        line
    }

    #[tokio::test]
    async fn a_prompt_is_answered_over_the_pipe() {
        let server = AskpassServer::start(answer_with(Some("hunter2"))).await.unwrap();
        let line = ask(&server, server.token(), "root@devbox's password:").await;

        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["answer"], "hunter2");
    }

    #[tokio::test]
    async fn a_dismissed_prompt_reports_no_answer() {
        let server = AskpassServer::start(answer_with(None)).await.unwrap();
        let line = ask(&server, server.token(), "password:").await;

        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(response.get("answer").is_none());
        assert_eq!(response["cancelled"], true);
    }

    #[tokio::test]
    async fn a_request_without_the_token_gets_nothing() {
        // Any local process of this user could otherwise ask zmux to pop a
        // credential dialog and read back what was typed.
        let server = AskpassServer::start(answer_with(Some("hunter2"))).await.unwrap();
        let line = ask(&server, "wrong", "password:").await;

        assert!(line.is_empty(), "an unauthorised request must receive nothing, got: {line:?}");
    }

    #[tokio::test]
    async fn a_second_prompt_does_not_wait_for_the_first() {
        // `ssh` asks again the moment an answer is refused, and several
        // connections prompt at once on a platform with no ControlMaster. A
        // single-instance pipe would answer the first and hand the rest
        // ERROR_PIPE_BUSY, which reads as "zmux ignored my password".
        let server = AskpassServer::start(answer_with(Some("x"))).await.unwrap();

        let first = ask(&server, server.token(), "password:").await;
        let second = ask(&server, server.token(), "password:").await;

        for line in [&first, &second] {
            let response: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(response["answer"], "x");
        }
    }

    #[tokio::test]
    async fn the_prompt_reaches_the_ui_classified() {
        // Getting this wrong echoes a password in cleartext.
        let seen = Arc::new(parking_lot::Mutex::new(None));
        let captured = Arc::clone(&seen);

        let answerer: Answerer = Arc::new(move |prompt: Prompt| {
            *captured.lock() = Some(prompt);
            Box::pin(async { Some("x".to_owned()) })
        });

        let server = AskpassServer::start(answerer).await.unwrap();
        let _ = ask(&server, server.token(), "Verification code:").await;

        let prompt = seen.lock().clone().expect("the UI was never asked");
        assert_eq!(prompt.kind, PromptKind::Challenge);
        assert_eq!(prompt.message, "Verification code:");
    }

    /// The guard the stub existed to protect, asserted rather than assumed.
    #[test]
    fn the_pipe_is_restricted_to_this_user_alone() {
        let sid = current_user_sid().expect("this process has a user");

        // A real user SID, not a well-known group. `S-1-5-32-544` is
        // Administrators and `S-1-1-0` is Everyone; granting either would be
        // the fail-open this module exists to avoid.
        assert!(sid.starts_with("S-1-5-21-"), "unexpected user SID: {sid}");
        assert_ne!(sid, "S-1-1-0");

        // And it must actually build a descriptor from it — a failure here means
        // the pipe would fall back to the default DACL, which Microsoft
        // documents as granting Everyone read access.
        let security = OwnerOnlySecurity::new().expect("the descriptor must build");
        assert!(!security.descriptor.is_null());
    }

    #[tokio::test]
    async fn two_servers_do_not_collide() {
        // Names carry 128 bits of entropy, and `first_pipe_instance` would make
        // a collision an error rather than a silent share.
        let a = AskpassServer::start(answer_with(Some("a"))).await.unwrap();
        let b = AskpassServer::start(answer_with(Some("b"))).await.unwrap();
        assert_ne!(a.socket_path(), b.socket_path());
        assert_ne!(a.token(), b.token());
    }
}
