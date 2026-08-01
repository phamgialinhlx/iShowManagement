//! Prints the tail of a folder's newest Claude transcript, as rmux parses it.
use rmux_claude::transcript;
use rmux_transport::{CommandSpec, Target, Tty};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let host = std::env::args().nth(1).unwrap();
    let folder = std::env::args().nth(2).unwrap();

    let target = rmux_ssh::SshTarget::new(rmux_transport::SshHostId::new(&host));
    target.connect().await?;

    let script = transcript::transcript_script(&folder, None, 256 * 1024);
    let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None);
    let out = target.exec(&spec).await?;
    let t = transcript::parse(out.stdout.as_bytes(), true);

    println!("session   : {}", t.session_id);
    println!("size      : {} bytes total, {} read", t.total_bytes, t.read_bytes);
    println!("entries   : {}", t.entries.len());
    println!("usage     : in {} out {} cache-read {} turns {}",
        t.usage.input, t.usage.output, t.usage.cache_read, t.usage.turns);
    println!("status    : mode={:?} perms={:?} model={:?} context={:?}",
        t.status.mode, t.status.permission_mode, t.status.model, t.status.context_tokens);
    println!("--- last 6 entries ---");
    for e in t.entries.iter().rev().take(6).rev() {
        let text: String = e.text.chars().take(110).collect();
        println!("[{:?}{}] {}", e.speaker,
            e.tool.as_deref().map(|t| format!("/{t}")).unwrap_or_default(),
            text.replace('\n', " ⏎ "));
    }
    Ok(())
}
