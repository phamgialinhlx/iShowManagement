//! Tiny networking helper: is a local TCP port accepting connections?
//! Used to probe readiness of port-forwards and SOCKS proxies. Mirrors
//! `references/tsmanager/server/net.js`.

use std::time::Duration;

use tokio::net::TcpStream;

/// True if something is listening on `127.0.0.1:<port>`.
pub async fn is_port_open(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(500),
            TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Ok(_))
    )
}
