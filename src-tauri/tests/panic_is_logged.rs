//! A panic must name itself in the log.
//!
//! This is the test for a real, expensive gap: a panic on a tokio worker
//! aborted the whole app (`panic = "abort"`), and `rmux.log` — the file the
//! operator exports when asked what happened — contained **nothing about it**.
//! The message goes to stderr, a Finder-launched `.app` has nowhere for stderr
//! to go, and the log only mirrors `tracing`. The single artefact was a macOS
//! crash report against a stripped binary, which symbolicates to nothing.
//!
//! Asserting on the *file* rather than on the hook being installed: "we called
//! `set_hook`" is true whether or not anything reaches the log, and the log is
//! the whole point.

use std::io::Read as _;

#[test]
fn a_panic_reaches_the_log_file_with_its_location() {
    let dir = std::env::temp_dir().join(format!("rmux-panic-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let file = rmux_lib::logs::writer(&dir).expect("open the log");

    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("rmux=debug"))
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(file))
        .init();

    rmux_lib::logs::route_panics_to_log();

    // A panic on a *named worker*, because that is the case that was
    // unattributable — the crash report's main thread was in AppKit and the
    // thread that actually died was a `tokio-rt-worker`.
    let handle = std::thread::Builder::new()
        .name("tokio-rt-worker".to_owned())
        .spawn(|| {
            // A **formatted** payload (`String`), which is what every real
            // `unwrap`/`expect` produces. A hook handling only `&'static str`
            // would silently drop exactly these — the ones that matter.
            let why = "a deliberate panic from the test".to_owned();
            panic!("{why}");
        })
        .unwrap();
    assert!(handle.join().is_err(), "the thread should have panicked");

    let mut text = String::new();
    std::fs::File::open(dir.join("logs").join("rmux.log"))
        .expect("the log file exists")
        .read_to_string(&mut text)
        .unwrap();

    assert!(
        text.contains("a deliberate panic from the test"),
        "the panic message must be in the log, got:\n{text}"
    );
    assert!(
        text.contains("panic_is_logged.rs"),
        "the panic's source location must be in the log, got:\n{text}"
    );
    assert!(
        text.contains("tokio-rt-worker"),
        "the thread name must be in the log — it is what tells two crashes apart, got:\n{text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
