//! Lists the Claude sessions recorded for a folder on this machine.
use rmux_transport::LocalTarget;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let folder = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    let folder = std::fs::canonicalize(&folder)?.to_string_lossy().into_owned();

    println!("sessions in {folder}:");
    for s in rmux_claude::ClaudeSession::list(&LocalTarget::new(), &folder).await? {
        println!("  {}  {}  {}", s.id, s.modified, s.title.unwrap_or_else(|| "—".into()));
    }
    Ok(())
}
