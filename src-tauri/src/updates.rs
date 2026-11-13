//! Telling the operator a newer rmux exists, and installing it when they say so.
//!
//! ## Two sources, one answer
//!
//! - **The signed updater manifest** (`latest.json` on the release marked
//!   *latest*, read by `tauri-plugin-updater`). When it lists this platform and
//!   the install can replace itself, the update is downloaded in the background,
//!   its minisign signature is checked against the public key in
//!   `tauri.conf.json`, and the strip offers **Restart to update**.
//! - **The GitHub release list**, as a fallback. A `.deb`/`.rpm` install cannot
//!   replace itself, a release may predate the manifest, and the manifest may not
//!   list Windows/Linux yet — in every one of those cases the operator is still
//!   told, with a **Download** link to the matching installer.
//!
//! ## Rules this module keeps
//!
//! - **Never restarts on its own.** The operator may be mid-sentence in a
//!   terminal. Installing is a click; sessions survive it because they live under
//!   `rmux-agent` on the target.
//! - **Install happens only at that click, never earlier.** Replacing the bundle
//!   while the old build runs would swap `Resources/agents` underneath it — the
//!   running app would then upload agents of the *new* version and refuse them
//!   ("the uploaded agent reported 0.2.28, expected 0.2.27"), so no terminal could
//!   open until a restart. Hence "Later" simply hides the strip for this run.
//! - **The webview never supplies a URL.** `update_open` takes a *kind* and opens
//!   the URL Rust took from GitHub's own response for that release.
//! - **One checker per app**, not per window: the state lives in a managed store,
//!   and every change is broadcast to every window as [`EVENT`].
//! - **Background failures are quiet** (logged, never a dialog). Only "Check now"
//!   reports a failure, inline beside its button.
//! - **Off means off**: with automatic checks disabled no timer runs and no
//!   request is made; a manual check still works.
//! - No process is spawned here (HTTP is in-process), except opening the browser,
//!   which goes through `auth::open_external` and its `CREATE_NO_WINDOW`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tauri_plugin_updater::UpdaterExt;

/// The repository releases are published to.
pub const REPO: &str = "phamgialinhlx/iShowManagement";
/// Event every window listens on for the current [`Status`].
pub const EVENT: &str = "rmux-update";
/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const FIRST_CHECK: Duration = Duration::from_secs(10);
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Progress events at most ~4/s; a dot does not need more.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

fn releases_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases?per_page=20")
}

fn release_page(tag: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/{tag}")
}

/* ------------------------------------------------------------------ pure part */

/// Which release stream this build follows. macOS releases are `rmux-vX.Y.Z`;
/// Windows and Linux share a separate `rmux-vX.Y.Z-win-linux` release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Mac,
    WinLinux,
}

impl Stream {
    pub fn this_build() -> Self {
        if cfg!(target_os = "macos") { Self::Mac } else { Self::WinLinux }
    }
}

/// Which installer to point a Download at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn this_build() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mac => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

pub type Version = (u64, u64, u64);

/// `X.Y.Z`, digits only. Anything else — a pre-release suffix, a fourth part —
/// is not a version this checker offers.
pub fn parse_version(s: &str) -> Option<Version> {
    let mut parts = s.split('.');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = (next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(v)
}

/// The version a tag names *for this stream*, or `None`. A `-win-linux` tag is
/// never a macOS release and a bare tag is never a Windows/Linux one.
pub fn tag_version(tag: &str, stream: Stream) -> Option<Version> {
    let rest = tag.strip_prefix("rmux-v")?;
    let core = match stream {
        Stream::Mac => rest,
        Stream::WinLinux => rest.strip_suffix("-win-linux")?,
    };
    parse_version(core)
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

/// A newer release, with the two URLs the strip may open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub version: String,
    pub tag: String,
    pub notes_url: String,
    pub download_url: String,
}

/// Only GitHub's own pages and downloads are ever opened from here.
fn trusted(url: &str) -> bool {
    url.starts_with("https://github.com/")
}

/// The installer a person on `os`/`arch` would download.
pub fn pick_asset<'a>(assets: &'a [Asset], os: Os, arch: &str) -> Option<&'a Asset> {
    let ends = |suffix: &str| assets.iter().find(|a| a.name.ends_with(suffix));
    match os {
        Os::Mac => assets.iter().find(|a| a.name.ends_with(".dmg") && a.name.contains(arch)),
        Os::Windows => ends("-setup.exe").or_else(|| ends(".msi")),
        Os::Linux => ends(".AppImage").or_else(|| ends(".deb")),
    }
}

/// The newest non-draft, non-prerelease release of `stream` that is strictly
/// newer than `current`.
pub fn newest(releases: &[Release], current: &str, stream: Stream, os: Os, arch: &str) -> Option<Offer> {
    let current = parse_version(current)?;
    let (version, release) = releases
        .iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| tag_version(&r.tag_name, stream).map(|v| (v, r)))
        .filter(|(v, _)| *v > current)
        .max_by_key(|(v, _)| *v)?;

    let notes_url = if trusted(&release.html_url) {
        release.html_url.clone()
    } else {
        release_page(&release.tag_name)
    };
    let download_url = pick_asset(&release.assets, os, arch)
        .map(|a| a.browser_download_url.clone())
        .filter(|u| trusted(u))
        .unwrap_or_else(|| notes_url.clone());

    Some(Offer {
        version: format!("{}.{}.{}", version.0, version.1, version.2),
        tag: release.tag_name.clone(),
        notes_url,
        download_url,
    })
}

/// Whether this install can replace itself. A `.deb`/`.rpm`/`.msi` cannot (or
/// must not be replaced by another format's installer); an unbundled dev binary
/// has nothing to replace.
fn self_update_supported() -> bool {
    use tauri::utils::config::BundleType;
    match Os::this_build() {
        Os::Mac => true,
        Os::Windows => matches!(tauri::utils::platform::bundle_type(), Some(BundleType::Nsis)),
        Os::Linux => {
            std::env::var_os("APPIMAGE").is_some()
                && matches!(tauri::utils::platform::bundle_type(), Some(BundleType::AppImage))
        }
    }
}

/* ------------------------------------------------------------------ state */

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Nothing newer is known.
    #[default]
    Idle,
    /// A newer release exists; this install can only be pointed at it.
    Available,
    /// Downloading a signed update in the background.
    Downloading,
    /// Downloaded and verified; one click installs and restarts.
    Ready,
}

/// What every window renders. Carries no URL — those stay in Rust.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub current: &'static str,
    pub platform: &'static str,
    pub phase: Phase,
    pub version: Option<String>,
    pub done: u64,
    pub total: Option<u64>,
    /// Whether this install can install updates itself.
    pub self_update: bool,
    /// "Later" was pressed this run.
    pub dismissed: bool,
    pub checking: bool,
    pub auto: bool,
    /// Why a signed update could not be fetched, when the strip fell back to a
    /// Download link.
    pub install_error: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            current: CURRENT,
            platform: Os::this_build().label(),
            phase: Phase::Idle,
            version: None,
            done: 0,
            total: None,
            self_update: self_update_supported(),
            dismissed: false,
            checking: false,
            auto: false,
            install_error: None,
        }
    }
}

#[derive(Default)]
struct Inner {
    status: Status,
    /// URLs for the strip, from GitHub's response.
    notes_url: Option<String>,
    download_url: Option<String>,
    /// ETag of the last release list, and the list it described.
    etag: Option<String>,
    releases: Vec<Release>,
}

#[derive(Default)]
pub struct UpdateStore {
    inner: Mutex<Inner>,
    /// One check at a time, app-wide.
    running: tokio::sync::Mutex<()>,
    /// The downloaded, verified update awaiting a restart.
    ready: tokio::sync::Mutex<Option<(tauri_plugin_updater::Update, Arc<Vec<u8>>)>>,
    timer: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl UpdateStore {
    fn snapshot(&self) -> Status {
        self.inner.lock().status.clone()
    }
}

fn broadcast<R: Runtime>(app: &AppHandle<R>, status: &Status) {
    if let Err(e) = app.emit(EVENT, status) {
        tracing::debug!(error = %e, "could not broadcast the update status");
    }
}

fn change<R: Runtime>(app: &AppHandle<R>, f: impl FnOnce(&mut Inner)) {
    let store = app.state::<UpdateStore>();
    let status = {
        let mut inner = store.inner.lock();
        f(&mut inner);
        inner.status.clone()
    };
    broadcast(app, &status);
}

/* ------------------------------------------------------------------ checking */

async fn fetch_releases(store: &UpdateStore) -> Result<Vec<Release>, String> {
    use reqwest::header::{ACCEPT, ETAG, IF_NONE_MATCH};

    let client = reqwest::Client::builder()
        .user_agent(format!("rmux/{CURRENT}"))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client
        .get(releases_url())
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    let etag = store.inner.lock().etag.clone();
    if let Some(etag) = etag {
        request = request.header(IF_NONE_MATCH, etag);
    }

    let response = request.send().await.map_err(|e| e.without_url().to_string())?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(store.inner.lock().releases.clone());
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(format!("GitHub is rate-limiting this network (HTTP {})", status.as_u16()));
    }
    if !status.is_success() {
        return Err(format!("GitHub answered HTTP {}", status.as_u16()));
    }
    let etag = response.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let releases: Vec<Release> =
        response.json().await.map_err(|e| format!("unreadable release list: {}", e.without_url()))?;

    let mut inner = store.inner.lock();
    inner.etag = etag;
    inner.releases = releases.clone();
    Ok(releases)
}

/// Download a signed update, reporting progress, and park it for the restart.
async fn download<R: Runtime>(app: &AppHandle<R>, update: tauri_plugin_updater::Update) -> Result<(), String> {
    let version = update.version.clone();
    let tag = format!("rmux-v{version}");
    change(app, |inner| {
        inner.status.phase = Phase::Downloading;
        inner.status.version = Some(version.clone());
        inner.status.done = 0;
        inner.status.total = None;
        inner.status.install_error = None;
        inner.notes_url = Some(release_page(&tag));
        inner.download_url = None;
    });

    let mut done: u64 = 0;
    let mut last = Instant::now();
    let progress_app = app.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                done += chunk as u64;
                if last.elapsed() >= PROGRESS_EVERY {
                    last = Instant::now();
                    change(&progress_app, |inner| {
                        inner.status.done = done;
                        inner.status.total = total;
                    });
                }
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;

    *app.state::<UpdateStore>().ready.lock().await = Some((update, Arc::new(bytes)));
    change(app, |inner| {
        inner.status.phase = Phase::Ready;
        inner.status.done = inner.status.total.unwrap_or(inner.status.done);
    });
    tracing::info!(%version, "update downloaded and verified; waiting for the operator to restart");
    Ok(())
}

async fn check_inner<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let mut install_error = None;

    if self_update_supported() {
        let checked = match app.updater_builder().timeout(Duration::from_secs(30)).build() {
            Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match checked {
            Ok(Some(update)) => match download(app, update).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // A signature failure lands here too. Fall through to the
                    // release list, so the operator still hears about it.
                    tracing::warn!(error = %e, "the signed update could not be downloaded");
                    install_error = Some(e);
                }
            },
            Ok(None) => tracing::debug!("updater manifest offers nothing newer"),
            Err(e) => tracing::info!(error = %e, "updater manifest unavailable; using the release list"),
        }
    }

    let store = app.state::<UpdateStore>();
    let releases = match fetch_releases(&store).await {
        Ok(r) => r,
        Err(e) => {
            if install_error.is_some() {
                change(app, |inner| {
                    inner.status.phase = Phase::Idle;
                    inner.status.install_error = install_error.clone();
                });
            }
            return Err(e);
        }
    };
    let offer = newest(&releases, CURRENT, Stream::this_build(), Os::this_build(), std::env::consts::ARCH);
    change(app, |inner| {
        inner.status.install_error = install_error;
        match offer {
            Some(offer) => {
                inner.status.phase = Phase::Available;
                inner.status.version = Some(offer.version);
                inner.notes_url = Some(offer.notes_url);
                inner.download_url = Some(offer.download_url);
            }
            None => {
                inner.status.phase = Phase::Idle;
                inner.status.version = None;
                inner.notes_url = None;
                inner.download_url = None;
            }
        }
    });
    Ok(())
}

/// One check. Serialised app-wide; a check while an update is downloading or
/// ready answers with that state rather than starting over.
pub async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Status, String> {
    let store = app.state::<UpdateStore>();
    let _one = store.running.lock().await;
    if matches!(store.snapshot().phase, Phase::Downloading | Phase::Ready) {
        return Ok(store.snapshot());
    }
    change(app, |inner| inner.status.checking = true);
    let result = check_inner(app).await;
    change(app, |inner| inner.status.checking = false);
    result.map(|()| store.snapshot())
}

fn start_timer<R: Runtime>(app: &AppHandle<R>) {
    let store = app.state::<UpdateStore>();
    let mut timer = store.timer.lock();
    if timer.is_some() {
        return;
    }
    let app = app.clone();
    *timer = Some(tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            match check(&app).await {
                Ok(status) => tracing::debug!(phase = ?status.phase, "background update check"),
                // Quiet by design: no network is not the operator's problem.
                Err(e) => tracing::info!(error = %e, "background update check failed"),
            }
            tokio::time::sleep(INTERVAL).await;
        }
    }));
}

fn stop_timer(store: &UpdateStore) {
    if let Some(handle) = store.timer.lock().take() {
        handle.abort();
    }
}

/* ------------------------------------------------------------------ commands */

/// The current state, for a window that just opened.
#[tauri::command]
pub fn update_status(store: State<'_, UpdateStore>) -> Status {
    store.snapshot()
}

/// Turn the background checker on or off. Every window sends its stored
/// preference on mount; repeating it is harmless.
#[tauri::command]
pub fn update_configure(app: AppHandle, store: State<'_, UpdateStore>, auto: bool) {
    if auto {
        start_timer(&app);
    } else {
        stop_timer(&store);
    }
    let changed = {
        let mut inner = store.inner.lock();
        let changed = inner.status.auto != auto;
        inner.status.auto = auto;
        changed
    };
    if changed {
        broadcast(&app, &store.snapshot());
    }
}

/// "Check now". Errors are returned for the button to show inline.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Status, String> {
    // A manual check undoes "Later": the operator just asked.
    change(&app, |inner| inner.status.dismissed = false);
    check(&app).await
}

/// Open the release notes or the installer for the release Rust found. The
/// webview names a kind, never a URL.
#[tauri::command]
pub async fn update_open(store: State<'_, UpdateStore>, kind: String) -> Result<(), String> {
    let url = {
        let inner = store.inner.lock();
        match kind.as_str() {
            "notes" => inner.notes_url.clone(),
            "download" => inner.download_url.clone().or_else(|| inner.notes_url.clone()),
            _ => return Err(format!("unknown link kind {kind:?}")),
        }
    };
    let url = url.ok_or("no newer release is known")?;
    crate::auth::open_external(url).await.map_err(|e| e.message)
}

/// "Later": hide the strip for this run.
#[tauri::command]
pub fn update_dismiss(app: AppHandle) {
    change(&app, |inner| inner.status.dismissed = true);
}

/// Install the downloaded update and restart. Only ever on the operator's click.
#[tauri::command]
pub async fn update_restart(app: AppHandle, store: State<'_, UpdateStore>) -> Result<(), String> {
    let Some((update, bytes)) = store.ready.lock().await.clone() else {
        return Err("no update has been downloaded".into());
    };
    // Extracting the bundle is blocking file work; keep it off the runtime.
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(bytes.as_slice()))
        .await
        .map_err(|e| e.to_string())?;
    installed.map_err(|e| format!("could not install the update: {e}"))?;
    app.request_restart();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
            draft: false,
            prerelease: false,
            assets: assets
                .iter()
                .map(|n| Asset {
                    name: (*n).into(),
                    browser_download_url: format!("https://github.com/{REPO}/releases/download/{tag}/{n}"),
                })
                .collect(),
        }
    }

    fn mac_list() -> Vec<Release> {
        vec![
            release("rmux-v0.2.27-win-linux", &["rmux_0.2.27_x64-setup.exe", "rmux_0.2.27_amd64.AppImage"]),
            release("rmux-v0.2.27", &["rmux_0.2.27_aarch64.dmg", "rmux.app.tar.gz"]),
            release("rmux-v0.2.26", &["rmux_0.2.26_aarch64.dmg"]),
        ]
    }

    #[test]
    fn versions_parse_strictly() {
        assert_eq!(parse_version("0.2.27"), Some((0, 2, 27)));
        assert_eq!(parse_version("0.2"), None);
        assert_eq!(parse_version("0.2.27.1"), None);
        assert_eq!(parse_version("0.2.x"), None);
        assert_eq!(parse_version("0.2.27-beta"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn tags_belong_to_one_stream() {
        assert_eq!(tag_version("rmux-v0.2.27", Stream::Mac), Some((0, 2, 27)));
        assert_eq!(tag_version("rmux-v0.2.27-win-linux", Stream::Mac), None);
        assert_eq!(tag_version("rmux-v0.2.27-win-linux", Stream::WinLinux), Some((0, 2, 27)));
        assert_eq!(tag_version("rmux-v0.2.27", Stream::WinLinux), None);
        for garbage in ["v0.2.27", "rmux-0.2.27", "rmux-vlatest", "rmux-v0.2.27-rc1", "other-v1.0.0"] {
            assert_eq!(tag_version(garbage, Stream::Mac), None, "{garbage}");
            assert_eq!(tag_version(garbage, Stream::WinLinux), None, "{garbage}");
        }
    }

    #[test]
    fn mac_is_offered_the_mac_release_and_its_dmg() {
        let offer = newest(&mac_list(), "0.2.26", Stream::Mac, Os::Mac, "aarch64").unwrap();
        assert_eq!(offer.version, "0.2.27");
        assert_eq!(offer.tag, "rmux-v0.2.27");
        assert!(offer.download_url.ends_with("/rmux-v0.2.27/rmux_0.2.27_aarch64.dmg"), "{offer:?}");
    }

    #[test]
    fn a_win_linux_release_is_never_offered_on_mac_and_vice_versa() {
        let only_win = vec![release("rmux-v0.3.0-win-linux", &["rmux_0.3.0_x64-setup.exe"])];
        assert_eq!(newest(&only_win, "0.2.27", Stream::Mac, Os::Mac, "aarch64"), None);
        let only_mac = vec![release("rmux-v0.3.0", &["rmux_0.3.0_aarch64.dmg"])];
        assert_eq!(newest(&only_mac, "0.2.27", Stream::WinLinux, Os::Windows, "x86_64"), None);
        assert_eq!(newest(&only_mac, "0.2.27", Stream::WinLinux, Os::Linux, "x86_64"), None);
    }

    #[test]
    fn an_equal_or_older_version_is_not_offered() {
        assert_eq!(newest(&mac_list(), "0.2.27", Stream::Mac, Os::Mac, "aarch64"), None);
        assert_eq!(newest(&mac_list(), "0.2.28", Stream::Mac, Os::Mac, "aarch64"), None);
    }

    #[test]
    fn versions_compare_numerically() {
        let list = vec![release("rmux-v0.2.9", &[]), release("rmux-v0.2.10", &[])];
        assert_eq!(newest(&list, "0.2.8", Stream::Mac, Os::Mac, "aarch64").unwrap().version, "0.2.10");
        assert_eq!(newest(&list, "0.2.9", Stream::Mac, Os::Mac, "aarch64").unwrap().version, "0.2.10");
    }

    #[test]
    fn drafts_prereleases_and_garbage_are_skipped() {
        let mut draft = release("rmux-v0.9.0", &[]);
        draft.draft = true;
        let mut pre = release("rmux-v0.8.0", &[]);
        pre.prerelease = true;
        let list = vec![draft, pre, release("nonsense", &[]), release("rmux-v0.2.28", &[])];
        assert_eq!(newest(&list, "0.2.27", Stream::Mac, Os::Mac, "aarch64").unwrap().version, "0.2.28");
    }

    #[test]
    fn windows_and_linux_get_their_installer_with_fallbacks() {
        let full = vec![release(
            "rmux-v0.2.28-win-linux",
            &["rmux_0.2.28_x64_en-US.msi", "rmux_0.2.28_x64-setup.exe", "rmux_0.2.28_amd64.deb", "rmux_0.2.28_amd64.AppImage"],
        )];
        let win = newest(&full, "0.2.27", Stream::WinLinux, Os::Windows, "x86_64").unwrap();
        assert!(win.download_url.ends_with("-setup.exe"), "{win:?}");
        let linux = newest(&full, "0.2.27", Stream::WinLinux, Os::Linux, "x86_64").unwrap();
        assert!(linux.download_url.ends_with(".AppImage"), "{linux:?}");

        let thin = vec![release("rmux-v0.2.28-win-linux", &["rmux_0.2.28_x64_en-US.msi", "rmux_0.2.28_amd64.deb"])];
        assert!(newest(&thin, "0.2.27", Stream::WinLinux, Os::Windows, "x86_64").unwrap().download_url.ends_with(".msi"));
        assert!(newest(&thin, "0.2.27", Stream::WinLinux, Os::Linux, "x86_64").unwrap().download_url.ends_with(".deb"));
    }

    #[test]
    fn no_matching_asset_falls_back_to_the_release_page() {
        let list = vec![release("rmux-v0.2.28", &["rmux_0.2.28_x86_64.dmg"])];
        let offer = newest(&list, "0.2.27", Stream::Mac, Os::Mac, "aarch64").unwrap();
        assert_eq!(offer.download_url, offer.notes_url);
        assert!(offer.notes_url.ends_with("/releases/tag/rmux-v0.2.28"));
    }

    #[test]
    fn only_github_urls_are_ever_opened() {
        let mut r = release("rmux-v0.2.28", &[]);
        r.html_url = "javascript:alert(1)".into();
        r.assets.push(Asset { name: "rmux_0.2.28_aarch64.dmg".into(), browser_download_url: "https://evil.example/x.dmg".into() });
        let offer = newest(&[r], "0.2.27", Stream::Mac, Os::Mac, "aarch64").unwrap();
        assert!(offer.notes_url.starts_with("https://github.com/"));
        assert!(offer.download_url.starts_with("https://github.com/"));
    }

    /// Hits the real GitHub API. `cargo test -p rmux --lib live_release_list -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_release_list() {
        let store = UpdateStore::default();
        let releases = fetch_releases(&store).await.expect("fetch releases");
        for current in ["0.2.26", "0.2.27"] {
            let offer = newest(&releases, current, Stream::Mac, Os::Mac, "aarch64");
            println!("mac, running {current}: {offer:?}");
        }
        for current in ["0.2.26", "0.2.27"] {
            println!("windows, running {current}: {:?}", newest(&releases, current, Stream::WinLinux, Os::Windows, "x86_64"));
            println!("linux, running {current}: {:?}", newest(&releases, current, Stream::WinLinux, Os::Linux, "x86_64"));
        }
        // And the conditional request: unchanged list costs a 304.
        let again = fetch_releases(&store).await.expect("refetch");
        println!("refetch with ETag {:?}: {} releases", store.inner.lock().etag, again.len());
    }
}
