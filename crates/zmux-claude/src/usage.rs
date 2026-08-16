//! Real usage, from the organisation's own records.
//!
//! Everything else zmux knows about spend is inferred from transcripts, which is
//! honest but local: it counts what *this* machine did. The Console Admin API
//! reports what the whole organisation actually consumed, which is the number
//! worth putting on a dashboard.
//!
//! This is deliberately **not** the subscription route. A `setup-token`
//! authenticates a Pro/Max plan and, by design, can only make model requests —
//! it cannot read usage at all. And the internal endpoint the CLI uses for
//! `/usage` rate-limits so aggressively that polling it is unusable. The Admin
//! API is documented, stable, and meant to be queried by tooling.
//!
//! It needs an **admin key** (`sk-ant-admin…`), which is a different and more
//! powerful thing than the key that runs models. It stays on this machine —
//! see [`crate::auth::CredentialKind`].

use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://api.anthropic.com/v1/organizations/usage_report/messages";
const API_VERSION: &str = "2023-06-01";

/// Usage over a window, already summed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    /// Input tokens that were not served from cache — the ones billed in full.
    pub uncached_input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub output: u64,
    /// Per-model totals of output tokens, largest first.
    pub by_model: Vec<ModelUsage>,
    /// How many days the figures cover.
    pub days: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub output: u64,
    pub input: u64,
}

// --- what the API actually returns ------------------------------------------
//
// Only the fields zmux uses are declared. The response gains fields between
// versions, and a strict shape would turn every such addition into a failure.

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    data: Vec<Bucket>,
}

#[derive(Deserialize)]
struct Bucket {
    #[serde(default)]
    results: Vec<ResultRow>,
}

#[derive(Deserialize)]
struct ResultRow {
    #[serde(default)]
    uncached_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize, Default)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
}

/// Sum a usage response into something displayable.
///
/// Separated from the request so it can be tested against a captured payload —
/// the shape is the part that breaks, and it cannot be exercised by a live call
/// in CI.
pub fn summarise(body: &str, days: u32) -> anyhow::Result<UsageReport> {
    let response: ApiResponse = serde_json::from_str(body)?;

    let mut report = UsageReport { days, ..Default::default() };
    let mut by_model: std::collections::BTreeMap<String, ModelUsage> = Default::default();

    for bucket in response.data {
        for row in bucket.results {
            let cache_creation = row
                .cache_creation
                .map(|c| c.ephemeral_1h_input_tokens + c.ephemeral_5m_input_tokens)
                .unwrap_or(0);

            report.uncached_input += row.uncached_input_tokens;
            report.cache_read += row.cache_read_input_tokens;
            report.cache_creation += cache_creation;
            report.output += row.output_tokens;

            // Rows are only labelled with a model when the query grouped by one;
            // an unlabelled row still counts toward the totals above.
            if let Some(model) = row.model {
                let entry = by_model.entry(model.clone()).or_insert(ModelUsage {
                    model,
                    output: 0,
                    input: 0,
                });
                entry.output += row.output_tokens;
                entry.input += row.uncached_input_tokens + row.cache_read_input_tokens;
            }
        }
    }

    report.by_model = by_model.into_values().collect();
    // Busiest first: with several models the interesting one is the one being
    // used, not the alphabetically first.
    report.by_model.sort_by_key(|m| std::cmp::Reverse(m.output));
    Ok(report)
}

/// The request zmux makes. Exposed so the URL can be asserted without a network.
pub fn request_url(starting_at: &str) -> String {
    // Daily buckets grouped by model: the smallest query that answers both
    // "how much have we spent" and "on what".
    format!("{ENDPOINT}?starting_at={starting_at}&bucket_width=1d&group_by[]=model&limit=31")
}

/// Fetch and summarise the last `days` days of organisation usage.
pub async fn fetch(admin_key: &str, days: u32, starting_at: &str) -> anyhow::Result<UsageReport> {
    anyhow::ensure!(
        matches!(
            crate::auth::CredentialKind::detect(admin_key),
            Some(crate::auth::CredentialKind::AdminKey)
        ),
        "the usage report needs an admin key (sk-ant-admin…), not a model key"
    );

    let client = reqwest::Client::new();
    let response = client
        .get(request_url(starting_at))
        .header("x-api-key", admin_key)
        .header("anthropic-version", API_VERSION)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    anyhow::ensure!(
        status.is_success(),
        "the usage API returned {status}: {}",
        // Bounded: an error body can be long, and this reaches a widget.
        body.chars().take(200).collect::<String>()
    );

    summarise(&body, days)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the documented example response.
    const SAMPLE: &str = r#"{
      "data": [
        {
          "ending_at": "2025-08-02T00:00:00Z",
          "starting_at": "2025-08-01T00:00:00Z",
          "results": [
            {
              "cache_creation": { "ephemeral_1h_input_tokens": 1000, "ephemeral_5m_input_tokens": 500 },
              "cache_read_input_tokens": 200,
              "model": "claude-opus-4-6",
              "output_tokens": 500,
              "uncached_input_tokens": 1500
            },
            {
              "cache_creation": { "ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0 },
              "cache_read_input_tokens": 50,
              "model": "claude-haiku-4-5",
              "output_tokens": 100,
              "uncached_input_tokens": 300
            }
          ]
        }
      ],
      "has_more": false
    }"#;

    #[test]
    fn a_documented_response_sums_correctly() {
        let report = summarise(SAMPLE, 7).unwrap();

        assert_eq!(report.uncached_input, 1800);
        assert_eq!(report.cache_read, 250);
        // Both cache tiers count toward what was written.
        assert_eq!(report.cache_creation, 1500);
        assert_eq!(report.output, 600);
        assert_eq!(report.days, 7);
    }

    #[test]
    fn models_are_ranked_by_how_much_they_produced() {
        let report = summarise(SAMPLE, 7).unwrap();

        assert_eq!(report.by_model.len(), 2);
        assert_eq!(report.by_model[0].model, "claude-opus-4-6");
        assert_eq!(report.by_model[0].output, 500);
        assert_eq!(report.by_model[1].model, "claude-haiku-4-5");
    }

    #[test]
    fn an_unfamiliar_field_does_not_break_the_summary() {
        // The response gains fields between API versions. Failing on one would
        // take the widget out for a change that does not concern it.
        let body = r#"{"data":[{"results":[{"output_tokens":5,"something_new":{"a":1}}]}],"brand_new":true}"#;
        assert_eq!(summarise(body, 1).unwrap().output, 5);
    }

    #[test]
    fn an_empty_report_is_zero_rather_than_an_error() {
        // A brand-new organisation, or a window with no traffic.
        let report = summarise(r#"{"data":[]}"#, 7).unwrap();
        assert_eq!(report, UsageReport { days: 7, ..Default::default() });
    }

    #[test]
    fn a_row_without_a_model_still_counts_toward_the_totals() {
        // `model` is null unless the query grouped by it. Dropping such a row
        // would silently under-report spend.
        let body = r#"{"data":[{"results":[{"output_tokens":42,"uncached_input_tokens":7}]}]}"#;
        let report = summarise(body, 1).unwrap();

        assert_eq!(report.output, 42);
        assert_eq!(report.uncached_input, 7);
        assert!(report.by_model.is_empty());
    }

    #[test]
    fn the_request_asks_for_daily_totals_per_model() {
        let url = request_url("2026-07-01T00:00:00Z");
        assert!(url.starts_with(ENDPOINT), "{url}");
        assert!(url.contains("bucket_width=1d"), "{url}");
        assert!(url.contains("group_by[]=model"), "{url}");
        assert!(url.contains("starting_at=2026-07-01T00:00:00Z"), "{url}");
    }

    #[tokio::test]
    async fn a_model_key_is_refused_before_any_request_is_made() {
        // Sending a model key here would fail anyway, but with an opaque 401.
        // More importantly the reverse mistake — an admin key used as a model
        // key — is the one that must never happen, so the two are kept distinct
        // at every boundary.
        let error = fetch("sk-ant-api03-not-an-admin-key", 7, "2026-07-01T00:00:00Z")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("admin key"), "{error}");
    }
}
