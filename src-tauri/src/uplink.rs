//! Where the target actually is: its addresses and its coordinates.
//!
//! Useful more often than it sounds. When you are running work across several
//! machines, "which box am I on and how do I reach it" is a question you answer
//! by hand a dozen times a day — the local address for a colleague on the same
//! network, the public one for a firewall rule, the city for latency you were
//! not expecting.
//!
//! ## The target makes the lookup, not this machine
//!
//! Geolocation is resolved **from the target**, over the connection rmux already
//! has. That is the only version that answers the right question: looking the
//! host up from here would report where *you* are, or where a NAT gateway is,
//! neither of which is the server.
//!
//! It does mean the target contacts a third-party service, which is a fact worth
//! being deliberate about rather than doing quietly:
//!
//! - It is fetched **once per host**, cached for the life of the run. Coordinates
//!   do not move, and polling someone else's free API is rude.
//! - It fails **silently and completely** — an air-gapped host, no `curl`, a
//!   blocked egress rule all just mean no coordinates, never an error the
//!   operator has to dismiss.
//! - Only the target's *own* public address is sent, which the service would see
//!   as the request's source address regardless.

use std::collections::HashMap;

use rmux_transport::{CommandSpec, Tty};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Mutex;

use crate::claude::ClaudeStore;
use crate::terminal::TargetRef;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Uplink {
    /// The address on the machine's own network — what a colleague on the same
    /// LAN would use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long: Option<f64>,
}

/// Cached per host. See the module note on why this is not polled.
#[derive(Default)]
pub struct UplinkStore {
    by_host: Mutex<HashMap<String, Uplink>>,
}

/// One round trip: local address, then the public lookup.
///
/// `ip route get` rather than `hostname -I`: a host with several interfaces
/// reports all of them, in an order that is not stable, and the one that matters
/// is the one it would actually route out of. macOS has no `ip`, hence the
/// `ipconfig`/`ifconfig` fallbacks.
const SCRIPT: &str = r#"
local=$(ip route get 1.1.1.1 2>/dev/null | sed -n 's/.*src \([0-9.]*\).*/\1/p' | head -1)
[ -n "$local" ] || local=$(ipconfig getifaddr en0 2>/dev/null)
[ -n "$local" ] || local=$(hostname -I 2>/dev/null | awk '{print $1}')
printf 'local=%s\n' "$local"
# 4 seconds, once. A host with no egress must not make the widget hang.
curl -s -m 4 https://ipinfo.io/json 2>/dev/null || true
"#;

#[tauri::command]
pub async fn host_uplink(
    store: State<'_, UplinkStore>,
    claude_store: State<'_, ClaudeStore>,
    target: TargetRef,
    // Ignore the cache — the operator pressed refresh.
    refresh: Option<bool>,
) -> Result<Uplink, String> {
    let key = target.host.clone().unwrap_or_else(|| "local".to_owned());

    if refresh != Some(true)
        && let Some(cached) = store.by_host.lock().await.get(&key)
    {
        return Ok(cached.clone());
    }

    let resolved = crate::claude::resolve(&claude_store, &target).await?;
    let out = resolved
        .exec(&CommandSpec::new("sh").arg("-c").arg(SCRIPT).tty(Tty::None))
        .await
        .map_err(|e| e.to_string())?;

    let uplink = parse(&out.stdout);
    store.by_host.lock().await.insert(key, uplink.clone());
    Ok(uplink)
}

/// Pull the fields out one at a time.
///
/// Deliberately not a `serde` struct over the JSON body. The response is a third
/// party's shape, it is mixed here with a line of our own, and a strict parse
/// would turn any change on their side into "no coordinates" — for a widget
/// where a missing field should simply be absent.
fn parse(text: &str) -> Uplink {
    let mut uplink = Uplink::default();

    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("local=") {
            let value = value.trim();
            if !value.is_empty() {
                uplink.local_ip = Some(value.to_owned());
            }
        }
    }

    let field = |name: &str| -> Option<String> {
        let needle = format!("\"{name}\"");
        let start = text.find(&needle)? + needle.len();
        let rest = text.get(start..)?;
        let rest = rest.trim_start().strip_prefix(':')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_owned())
    };

    uplink.public_ip = field("ip");
    uplink.city = field("city");
    uplink.country = field("country");

    // `"loc": "10.82,106.63"` — latitude first.
    if let Some(loc) = field("loc") {
        let mut parts = loc.split(',');
        uplink.lat = parts.next().and_then(|v| v.trim().parse().ok());
        uplink.long = parts.next().and_then(|v| v.trim().parse().ok());
    }

    uplink
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_the_address_and_the_coordinates() {
        let out = parse(
            "local=192.168.1.42\n{\"ip\":\"203.0.113.7\",\"city\":\"Ho Chi Minh City\",\
             \"country\":\"VN\",\"loc\":\"10.8231,106.6297\"}",
        );

        assert_eq!(out.local_ip.as_deref(), Some("192.168.1.42"));
        assert_eq!(out.public_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(out.city.as_deref(), Some("Ho Chi Minh City"));
        assert_eq!(out.lat, Some(10.8231));
        assert_eq!(out.long, Some(106.6297));
    }

    #[test]
    fn a_host_with_no_egress_still_reports_its_local_address() {
        // `curl` produced nothing at all. The widget should show the LAN address
        // and simply omit the rest, rather than reporting the whole lookup as a
        // failure — which is what a strict JSON parse over the whole body would
        // have done.
        let out = parse("local=10.0.0.5\n");

        assert_eq!(out.local_ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(out.public_ip, None);
        assert_eq!(out.lat, None);
    }

    #[test]
    fn an_unfamiliar_response_shape_loses_only_what_it_changed() {
        // The service renamed `loc` and dropped `city`. Everything still present
        // must survive: a strict deserialise would have discarded the address
        // too, for a field the widget does not need.
        let out = parse("local=10.0.0.5\n{\"ip\":\"203.0.113.7\",\"position\":\"1,2\"}");

        assert_eq!(out.public_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(out.city, None);
        assert_eq!(out.lat, None);
    }
}
