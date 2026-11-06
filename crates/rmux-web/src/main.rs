//! `rmux-web` — see `docs/rmux-web.md` and the crate docs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::process::ExitCode;

use rmux_web::{Options, cli, conns, host};

fn main() -> ExitCode {
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("rmux-web: {e}");
            return ExitCode::from(2);
        }
    };
    if cli.help {
        println!("{}", cli::USAGE);
        return ExitCode::SUCCESS;
    }
    // Before anything is created, bound or generated.
    if let Err(e) = cli::check_listen(cli.listen, cli.allow_non_loopback) {
        eprintln!("rmux-web: {e}");
        return ExitCode::from(2);
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("rmux-web: starting the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rmux-web: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: cli::Cli) -> anyhow::Result<()> {
    let token = match &cli.token_file {
        Some(path) => cli::read_token_file(path)?,
        None => cli::generate_token()?,
    };
    // Before binding: a verifier with no keys would refuse every Access
    // request, and nothing on screen would say why.
    let access = match (&cli.access_team, &cli.access_aud) {
        (Some(team), Some(aud)) => {
            let access = rmux_web::access::Access::connect(team, aud, &cli.access_emails).await?;
            access.spawn_periodic_refresh();
            let who = if access.emails().is_empty() {
                "any identity Access admits".to_owned()
            } else {
                access.emails().join(", ")
            };
            tracing::info!(team = access.team(), keys = access.key_count(), admits = %who, "Cloudflare Access verification on");
            Some(access)
        }
        _ => None,
    };
    let (router, _state) = rmux_web::build(Options {
        registry: rmux_app::registry(),
        // Absolute, always: a relative `--resources` became a relative program
        // path for the agent, which the PTY spawner then searched for on `PATH`
        // — so every terminal failed with "No viable candidates found in PATH"
        // when the server was started from its own directory.
        ui_dir: cli.ui.as_deref().map(absolute).transpose()?,
        resource_dir: cli.resources.as_deref().map(absolute).transpose()?,
        data_dir: host::default_data_dir()?,
        cache_dir: host::default_cache_dir()?,
        token: token.clone(),
        access,
        queue_capacity: conns::DEFAULT_QUEUE_CAPACITY,
    })?;

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .map_err(|e| anyhow::anyhow!("binding {}: {e}", cli.listen))?;
    let bound = listener.local_addr()?;
    if cli.ui.is_none() {
        tracing::warn!("no --ui given: serving /api only");
    }
    // A generated token is printed once, because nothing else knows it. A token
    // read from a file is never printed: whoever wrote the file already has it,
    // and stderr under a service manager lands in a log other accounts may read.
    if cli.token_file.is_some() {
        eprintln!("rmux-web: listening on http://{}/ (token from file)", display_addr(bound));
    } else {
        eprintln!("rmux-web: open http://{}/#token={token}", display_addr(bound));
    }
    drop(token);

    rmux_web::serve(listener, router, shutdown()).await?;
    tracing::info!("stopped");
    Ok(())
}

/// The address to put in the printed URL: an unspecified bind is reachable on
/// loopback, which is what the operator tunnels to.
fn display_addr(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port()),
        IpAddr::V6(ip) if ip.is_unspecified() => SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port()),
        _ => addr,
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// `path` made absolute against the working directory, and checked to exist.
fn absolute(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    std::fs::canonicalize(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}
