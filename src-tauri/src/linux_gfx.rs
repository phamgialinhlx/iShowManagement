//! Linux graphics workarounds, applied before the webview exists.
//!
//! WebKitGTK ≥ 2.40 composites through DMABUF buffers. On the proprietary
//! NVIDIA driver — Wayland especially — that path hands the driver buffers it
//! mishandles, and the webview shows garbled tiles, smeared canvases and stale
//! frames. Upstream's own escape hatch is `WEBKIT_DISABLE_DMABUF_RENDERER`,
//! which falls back to the shared-memory path: slower in principle, correct in
//! practice, and invisible on machines that never hit the bug because it is
//! only set when the NVIDIA kernel module is actually loaded.
//!
//! This must run before GTK initialises — WebKit reads the variable once, at
//! startup — which is why `run()` calls it before the builder, not in `setup()`.

pub fn apply_workarounds() {
    const VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";

    // An operator who set the variable themselves — either value — has already
    // decided; never overwrite a deliberate choice with a heuristic.
    if std::env::var_os(VAR).is_some() {
        tracing::info!("{VAR} already set by the environment; left as is");
        return;
    }

    if !nvidia_driver_loaded() {
        return;
    }

    unsafe { std::env::set_var(VAR, "1") };
    tracing::info!("NVIDIA driver detected; set {VAR}=1 to avoid corrupted webview rendering");
}

/// The proprietary module announces itself in both places; either alone is
/// enough. Nouveau does not create `/proc/driver/nvidia` and does not need the
/// workaround.
fn nvidia_driver_loaded() -> bool {
    std::path::Path::new("/proc/driver/nvidia/version").exists()
        || std::path::Path::new("/sys/module/nvidia").exists()
}
