use std::time::Instant;

use harness_core::HarnessError;

const ERROR_BODY_LIMIT: usize = 500;

/// Sends a request the caller has built (URL, auth headers, JSON body).
/// Returns the response text and the round-trip latency in milliseconds.
/// `provider` prefixes error messages.
pub(crate) async fn send_json(
    provider: &str,
    req: reqwest::RequestBuilder,
) -> Result<(String, u64), HarnessError> {
    let started = Instant::now();
    let resp = req
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
