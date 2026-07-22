//! iShowManagement server binary — a thin wrapper over [`ismcore::serve`].
//! (The desktop shell embeds the same library.)

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ismcore=info,tower_http=info".into()),
        )
        .init();

    if let Err(e) = ismcore::serve(ismcore::DEFAULT_PORT).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
