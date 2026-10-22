use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::HarnessError;
use crate::future::BoxFuture;

/// A capability the machine must be granted before an effect may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FileRead,
    HttpGet,
    DangerousShell,
    Composio,
}

/// A side effect executed by Rust. Named by a node in code, never by a model.
pub trait Effect: Send + Sync {
    fn name(&self) -> &str;
    fn capability(&self) -> Capability;
    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>>;
}
