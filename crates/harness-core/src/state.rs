use std::collections::BTreeMap;

use serde_json::Value;

/// Workflow state. Ordered so input hashes are stable across runs.
pub type State = BTreeMap<String, Value>;
