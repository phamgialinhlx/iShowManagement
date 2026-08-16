//! Prints the hosts zmux would offer, from this machine's real ssh config.
fn main() {
    for host in zmux_ssh::list_hosts() {
        println!(
            "{:<24} {:<28} {}",
            host.alias,
            host.hostname.unwrap_or_default(),
            host.user.unwrap_or_default()
        );
    }
}
