//! Prints the hosts rmux would offer, from this machine's real ssh config.
fn main() {
    for host in rmux_ssh::list_hosts() {
        println!(
            "{:<24} {:<28} {}",
            host.alias,
            host.hostname.unwrap_or_default(),
            host.user.unwrap_or_default()
        );
    }
}
