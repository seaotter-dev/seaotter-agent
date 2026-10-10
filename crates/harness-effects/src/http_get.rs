use std::collections::BTreeSet;
use std::error::Error as _;
use std::sync::Arc;
use std::time::Duration;

use harness_core::{BoxFuture, Capability, Effect, HarnessError};
use reqwest::Url;
use reqwest::redirect::Policy;
use serde_json::{Value, json};

pub const DEFAULT_NAME: &str = "http_get";
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_REDIRECTS: usize = 5;

/// GETs URLs whose host is on an explicit allowlist. Input is
/// `{"url": "...", "method"?: "GET"}`; output is
/// `{"url", "status", "content_type", "body"}` for any status.
///
/// Every check runs before a request is sent, and redirects are followed only
/// when each hop passes the same checks.
#[derive(Debug, Clone)]
pub struct HttpGet {
    name: String,
    allowlist: Arc<BTreeSet<String>>,
    allow_http: bool,
    max_bytes: u64,
    timeout: Duration,
    client: reqwest::Client,
}

impl HttpGet {
    /// Hosts are matched exactly and case-insensitively against the URL's
    /// host (as `Url::host_str` renders it, e.g. `[::1]` for IPv6). Ports are
    /// not part of the match.
    pub fn new<I, S>(hosts: I) -> Result<Self, HarnessError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let allowlist: BTreeSet<String> = hosts
            .into_iter()
            .map(|h| h.as_ref().trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if allowlist.is_empty() {
            return Err(HarnessError::Effect(
                "http_get: the host allowlist is empty".into(),
            ));
        }
        let allowlist = Arc::new(allowlist);
        let client = build_client(&allowlist, false, DEFAULT_TIMEOUT)?;
        Ok(Self {
            name: DEFAULT_NAME.into(),
            allowlist,
            allow_http: false,
            max_bytes: DEFAULT_MAX_BYTES,
            timeout: DEFAULT_TIMEOUT,
            client,
        })
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Allows plain `http` URLs (and redirect hops). Off by default.
    pub fn allow_http(mut self, yes: bool) -> Result<Self, HarnessError> {
        self.allow_http = yes;
        self.client = build_client(&self.allowlist, yes, self.timeout)?;
        Ok(self)
    }

    pub fn max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Result<Self, HarnessError> {
        self.timeout = timeout;
        self.client = build_client(&self.allowlist, self.allow_http, timeout)?;
        Ok(self)
    }

    fn err(&self, msg: impl std::fmt::Display) -> HarnessError {
        HarnessError::Effect(format!("{}: {msg}", self.name))
    }

    /// Validates the input without any network I/O and returns the URL to GET.
    pub fn check(&self, input: &Value) -> Result<Url, HarnessError> {
        match input.get("method") {
            None => {}
            Some(Value::String(m)) if m.eq_ignore_ascii_case("GET") => {}
            Some(other) => {
                let shown = other
                    .as_str()
                    .map_or_else(|| other.to_string(), str::to_owned);
                return Err(self.err(format!("method `{shown}` is not allowed")));
            }
        }
        let raw = input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| self.err("input needs a string `url`"))?;
        let url = Url::parse(raw).map_err(|e| self.err(format!("bad url `{raw}`: {e}")))?;
        url_allowed(&url, &self.allowlist, self.allow_http).map_err(|msg| self.err(msg))?;
        Ok(url)
    }

    async fn get(&self, input: Value) -> Result<Value, HarnessError> {
        let url = self.check(&input)?;
        let mut resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| self.err(describe(&e)))?;
        if resp
            .content_length()
            .is_some_and(|len| len > self.max_bytes)
        {
            return Err(self.err(format!(
                "response is over the {} byte limit",
                self.max_bytes
            )));
        }
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| self.err(describe(&e)))? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > self.max_bytes {
                return Err(self.err(format!(
                    "response is over the {} byte limit",
                    self.max_bytes
                )));
            }
        }
        let body =
            String::from_utf8(body).map_err(|_| self.err("response body is not UTF-8 text"))?;
        Ok(json!({
            "url": final_url,
            "status": status,
            "content_type": content_type,
            "body": body,
        }))
    }
}

/// Scheme, userinfo, and host rules shared by the pre-send check and every
/// redirect hop.
fn url_allowed(url: &Url, allowlist: &BTreeSet<String>, allow_http: bool) -> Result<(), String> {
    match url.scheme() {
        "https" => {}
        "http" if allow_http => {}
        "http" => return Err(format!("`{url}` uses http, which is not allowed")),
        other => return Err(format!("scheme `{other}` is not allowed")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("urls with credentials are not allowed".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("`{url}` has no host"))?
        .to_ascii_lowercase();
    if !allowlist.contains(&host) {
        return Err(format!("host `{host}` is not on the allowlist"));
    }
    Ok(())
}

fn build_client(
    allowlist: &Arc<BTreeSet<String>>,
    allow_http: bool,
    timeout: Duration,
) -> Result<reqwest::Client, HarnessError> {
    let allowlist = Arc::clone(allowlist);
    let policy = Policy::custom(move |attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error(format!("more than {MAX_REDIRECTS} redirects"));
        }
        match url_allowed(attempt.url(), &allowlist, allow_http) {
            Ok(()) => attempt.follow(),
            Err(msg) => attempt.error(format!("redirect refused: {msg}")),
        }
    });
    reqwest::Client::builder()
        .redirect(policy)
        .timeout(timeout)
        .build()
        .map_err(|e| HarnessError::Effect(format!("http_get client: {e}")))
}

/// A reqwest error with its source chain, so redirect refusals stay visible.
fn describe(err: &reqwest::Error) -> String {
    let mut msg = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        msg.push_str(": ");
        msg.push_str(&cause.to_string());
        source = cause.source();
    }
    msg
}

impl Effect for HttpGet {
    fn name(&self) -> &str {
        &self.name
    }

    fn capability(&self) -> Capability {
        Capability::HttpGet
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(self.get(input))
    }
}
