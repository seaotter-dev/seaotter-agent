use std::sync::Arc;

use harness_core::{BoxFuture, Capability, Effect, HarnessError};
use serde_json::{Value, json};

pub const DEFAULT_NAME: &str = "composio";
const EXECUTE_ROOT: &str = "https://backend.composio.dev/api/v3.1/tools/execute";

/// One execute call. The API key is present only so the live transport can
/// set `x-api-key`. It is not part of the recorded request body.
pub struct ExecuteCall {
    pub action: String,
    pub connection: String,
    pub arguments: Value,
    pub key: String,
}

enum KeySource {
    Environment,
    Fixed(Option<String>),
}

type Transport =
    Arc<dyn Fn(ExecuteCall) -> BoxFuture<'static, Result<Value, HarnessError>> + Send + Sync>;

/// Runs one granted Composio action. The workflow file stores the connection
/// id and the action name. `COMPOSIO_API_KEY` stays in the environment.
pub struct Composio {
    name: String,
    action: String,
    key: KeySource,
    transport: Transport,
}

impl Composio {
    /// `action` is the single granted action name.
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            name: DEFAULT_NAME.into(),
            action: action.into(),
            key: KeySource::Environment,
            transport: Arc::new(live_transport),
        }
    }

    /// `None` is a missing key and does not read the environment.
    pub fn with_key(mut self, key: Option<String>) -> Self {
        self.key = KeySource::Fixed(key);
        self
    }

    pub fn with_transport<F>(mut self, transport: F) -> Self
    where
        F: Fn(ExecuteCall) -> BoxFuture<'static, Result<Value, HarnessError>>
            + Send
            + Sync
            + 'static,
    {
        self.transport = Arc::new(transport);
        self
    }

    fn missing_key(&self) -> HarnessError {
        HarnessError::Effect(format!("{}: COMPOSIO_API_KEY is not set", self.name))
    }

    async fn execute(&self, input: Value) -> Result<Value, HarnessError> {
        let requested = input
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if requested != self.action {
            return Err(HarnessError::Workflow(format!(
                "effect `composio` action `{requested}` has no grant"
            )));
        }
        let connection = input
            .get("connection")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if connection.is_empty() {
            return Err(HarnessError::Workflow(
                "effect `composio` has no connection".into(),
            ));
        }
        let key = match &self.key {
            KeySource::Fixed(Some(key)) if !key.trim().is_empty() => key.clone(),
            KeySource::Fixed(_) => return Err(self.missing_key()),
            KeySource::Environment => match std::env::var("COMPOSIO_API_KEY") {
                Ok(key) if !key.trim().is_empty() => key,
                _ => return Err(self.missing_key()),
            },
        };
        let mut arguments = input;
        if let Some(object) = arguments.as_object_mut() {
            object.remove("connection");
            object.remove("action");
        }
        (self.transport)(ExecuteCall {
            action: requested,
            connection,
            arguments,
            key,
        })
        .await
    }
}

fn live_transport(call: ExecuteCall) -> BoxFuture<'static, Result<Value, HarnessError>> {
    Box::pin(async move {
        let url = format!("{EXECUTE_ROOT}/{}", call.action);
        let response = reqwest::Client::new()
            .post(&url)
            .header("x-api-key", &call.key)
            .json(&json!({
                "connected_account_id": call.connection,
                "arguments": call.arguments,
            }))
            .send()
            .await
            .map_err(|err| HarnessError::Effect(format!("composio: {err}")))?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|err| {
            HarnessError::Effect(format!("composio: response is not JSON: {err}"))
        })?;
        if !status.is_success() {
            return Err(HarnessError::Effect(format!("composio: HTTP {status}")));
        }
        Ok(body)
    })
}

impl Effect for Composio {
    fn name(&self) -> &str {
        &self.name
    }

    fn capability(&self) -> Capability {
        Capability::Composio
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(self.execute(input))
    }
}
