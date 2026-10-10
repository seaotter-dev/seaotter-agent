use std::time::Instant;

use harness_core::HarnessError;
use serde::Serialize;

const ERROR_BODY_LIMIT: usize = 500;

/// POSTs `body` as JSON with Bearer auth. Returns the response text and the
/// round-trip latency in milliseconds. `provider` prefixes error messages.
pub(crate) async fn post_json<B: Serialize + ?Sized>(
    client: &reqwest::Client,
    provider: &str,
    url: &str,
    api_key: &str,
    body: &B,
) -> Result<(String, u64), HarnessError> {
    let started = Instant::now();
    let resp = client
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|e| HarnessError::Provider(format!("{provider} request: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| HarnessError::Provider(format!("{provider} response: {e}")))?;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if !status.is_success() {
        let snippet: String = text.chars().take(ERROR_BODY_LIMIT).collect();
        return Err(HarnessError::Provider(format!(
            "{provider} returned {status}: {snippet}"
        )));
    }
    Ok((text, latency_ms))
}
