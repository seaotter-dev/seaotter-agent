use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::document::RiskTier;
use crate::error::HarnessError;
use crate::node::NodeId;

/// Whether a document effect's human gate allowed the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum GateOutcome {
    Passed,
    Skipped,
    NotRequired,
}

/// The owner, tier, tool, and gate outcome for one effect.
///
/// A host permission and a credential are not fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantDecision {
    pub owner: String,
    pub tier: RiskTier,
    pub tool: String,
    pub gate: GateOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Decide,
    Generate,
    Effect,
}

/// Latency and cost reported for one call. Unknown values are `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub latency_ms: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
}

/// One append-only record of a decide, generate, or effect step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub node: NodeId,
    /// Node id for a decide or generate step. The effect's registered name
    /// for an effect step.
    pub name: String,
    pub kind: EventKind,
    pub input_hash: String,
    pub output: Value,
    pub confidence: Option<f64>,
    pub usage: Usage,
}

/// Append-only log of decide, generate, and effect steps, plus one grant
/// decision per document effect that was skipped or run.
///
/// Events can be read. Nothing in this type updates or removes one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventLog {
    events: Vec<Event>,
    grants: Vec<GrantDecision>,
}

impl EventLog {
    pub fn as_slice(&self) -> &[Event] {
        &self.events
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Event> {
        self.events.iter()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn grants(&self) -> &[GrantDecision] {
        &self.grants
    }

    pub(crate) fn push(&mut self, event: Event) {
        self.events.push(event);
    }

    pub(crate) fn push_grant(&mut self, decision: GrantDecision) {
        self.grants.push(decision);
    }

    /// Appends this log to `path`. The header and step lines match
    /// [`append_run`]. Each grant decision is one later line with `run_id`,
    /// `owner`, `tier`, `tool`, and `gate`.
    pub fn append_to(
        &self,
        path: &Path,
        workflow: &str,
        user_id: &str,
    ) -> Result<String, HarnessError> {
        write_run(path, workflow, user_id, &self.events, &self.grants)
    }
}

/// Hex SHA-256 of `value` serialized as JSON. Map keys are ordered, so the
/// same value always hashes the same.
pub fn input_hash(value: &impl Serialize) -> Result<String, HarnessError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| HarnessError::Workflow(format!("event input: {e}")))?;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

fn new_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let n = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{n}-{}", std::process::id())
}

#[derive(Serialize)]
struct RunHeader<'a> {
    run_id: &'a str,
    workflow: &'a str,
    user_id: &'a str,
}

#[derive(Serialize)]
struct RunStep<'a> {
    run_id: &'a str,
    kind: EventKind,
    name: &'a str,
    node: &'a NodeId,
    input_hash: &'a str,
    outcome: &'a Value,
    confidence: Option<f64>,
    usage: &'a Usage,
}

#[derive(Deserialize)]
struct StoredStep {
    kind: EventKind,
    name: String,
    node: NodeId,
    input_hash: String,
    outcome: Value,
    confidence: Option<f64>,
    usage: Usage,
}

#[derive(Serialize)]
struct GrantLine<'a> {
    run_id: &'a str,
    owner: &'a str,
    tier: RiskTier,
    tool: &'a str,
    gate: GateOutcome,
}

/// Appends one run to `path` as JSON lines. The first line is the header.
/// Each later line is one decide, generate, or effect step. The write is a
/// single append. This function does not read provider keys.
pub fn append_run(
    path: &Path,
    workflow: &str,
    user_id: &str,
    events: &[Event],
) -> Result<String, HarnessError> {
    write_run(path, workflow, user_id, events, &[])
}

fn write_run(
    path: &Path,
    workflow: &str,
    user_id: &str,
    events: &[Event],
    grants: &[GrantDecision],
) -> Result<String, HarnessError> {
    let run_id = new_run_id();
    let mut body = json_line(&RunHeader {
        run_id: &run_id,
        workflow,
        user_id,
    })?;
    for event in events {
        body.push_str(&json_line(&RunStep {
            run_id: &run_id,
            kind: event.kind,
            name: &event.name,
            node: &event.node,
            input_hash: &event.input_hash,
            outcome: &event.output,
            confidence: event.confidence,
            usage: &event.usage,
        })?);
    }
    for decision in grants {
        body.push_str(&json_line(&GrantLine {
            run_id: &run_id,
            owner: &decision.owner,
            tier: decision.tier,
            tool: &decision.tool,
            gate: decision.gate,
        })?);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|err| {
            HarnessError::Workflow(format!("creating {}: {err}", parent.display()))
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| HarnessError::Workflow(format!("opening {}: {err}", path.display())))?;
    file.write_all(body.as_bytes())
        .map_err(|err| HarnessError::Workflow(format!("writing {}: {err}", path.display())))?;
    Ok(run_id)
}

/// Events for `run_id`. Lines from other runs are ignored.
pub fn read_run(path: &Path, run_id: &str) -> Result<Vec<Event>, HarnessError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| HarnessError::Workflow(format!("reading {}: {err}", path.display())))?;
    let mut seen = false;
    let mut events = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|err| HarnessError::Workflow(format!("run log: {err}")))?;
        if value.get("run_id").and_then(Value::as_str) != Some(run_id) {
            continue;
        }
        seen = true;
        if value.get("kind").is_none() {
            continue;
        }
        let step: StoredStep = serde_json::from_value(value)
            .map_err(|err| HarnessError::Workflow(format!("run log step: {err}")))?;
        events.push(Event {
            node: step.node,
            name: step.name,
            kind: step.kind,
            input_hash: step.input_hash,
            output: step.outcome,
            confidence: step.confidence,
            usage: step.usage,
        });
    }
    if !seen {
        return Err(HarnessError::Workflow(format!("unknown run `{run_id}`")));
    }
    Ok(events)
}

fn json_line(value: &impl Serialize) -> Result<String, HarnessError> {
    let mut line = serde_json::to_string(value)
        .map_err(|err| HarnessError::Workflow(format!("run log: {err}")))?;
    line.push('\n');
    Ok(line)
}
