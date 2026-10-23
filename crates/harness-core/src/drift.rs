//! Prior runs of one workflow, read from the append-only log.
//!
//! This module loads history. It does not compare a new run with that history.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::error::HarnessError;
use crate::event::{Event, EventKind, Signal, Usage};
use crate::node::NodeId;

/// One earlier run of the workflow named in the log header.
#[derive(Debug, Clone, PartialEq)]
pub struct PriorRun {
    pub run_id: String,
    pub events: Vec<Event>,
}

/// Prior runs of one workflow, excluding the current run.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    runs: Vec<PriorRun>,
}

impl Baseline {
    pub fn runs(&self) -> &[PriorRun] {
        &self.runs
    }

    /// One `baseline_missing` signal when no prior runs remain.
    ///
    /// A loaded run produces no signal and no drift finding.
    pub fn missing_signal(&self, node: &NodeId) -> Option<Signal> {
        if !self.runs.is_empty() {
            return None;
        }
        Some(Signal {
            check: "baseline_missing".into(),
            node: node.clone(),
            finding: "no prior runs".into(),
        })
    }
}

/// Reads prior runs of `workflow` from the append-only log at `path`.
///
/// A missing file is an empty baseline. `current_run_id` is left out even
/// when its header names `workflow`. Grant lines and signal lines are not
/// steps.
pub fn load_baseline(
    path: &Path,
    workflow: &str,
    current_run_id: &str,
) -> Result<Baseline, HarnessError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Baseline { runs: Vec::new() });
        }
        Err(err) => {
            return Err(HarnessError::Workflow(format!(
                "reading {}: {err}",
                path.display()
            )));
        }
    };

    let mut order = Vec::new();
    let mut partials: BTreeMap<String, PartialRun> = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|err| HarnessError::Workflow(format!("run log: {err}")))?;
        let Some(run_id) = value.get("run_id").and_then(Value::as_str) else {
            continue;
        };
        if !partials.contains_key(run_id) {
            order.push(run_id.to_owned());
            partials.insert(run_id.to_owned(), PartialRun::default());
        }
        let partial = partials.get_mut(run_id).expect("run inserted above");
        if value.get("kind").is_some() {
            let step: StoredStep = serde_json::from_value(value)
                .map_err(|err| HarnessError::Workflow(format!("run log step: {err}")))?;
            partial.events.push(Event {
                node: step.node,
                name: step.name,
                kind: step.kind,
                input_hash: step.input_hash,
                output: step.outcome,
                confidence: step.confidence,
                usage: step.usage,
            });
        } else if let Some(name) = value.get("workflow").and_then(Value::as_str) {
            partial.workflow = Some(name.to_owned());
        }
    }

    let runs = order
        .into_iter()
        .filter(|run_id| run_id != current_run_id)
        .filter_map(|run_id| {
            let partial = partials.remove(&run_id)?;
            if partial.workflow.as_deref() != Some(workflow) {
                return None;
            }
            Some(PriorRun {
                run_id,
                events: partial.events,
            })
        })
        .collect();
    Ok(Baseline { runs })
}

#[derive(Default)]
struct PartialRun {
    workflow: Option<String>,
    events: Vec<Event>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, append_run};
    use serde_json::json;

    fn step(node: &str, confidence: f64) -> Event {
        Event {
            node: node.into(),
            name: node.into(),
            kind: EventKind::Decide,
            input_hash: "abc".into(),
            output: json!({"route": "billing"}),
            confidence: Some(confidence),
            usage: Usage::default(),
        }
    }

    fn log_path(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("seaotter-baseline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_baseline_keeps_other_runs_of_the_same_workflow() {
        let path = log_path("kept");
        let earlier = append_run(&path, "ticket", "user-7", &[step("classify", 0.9)]).unwrap();
        let current = append_run(&path, "ticket", "user-7", &[step("classify", 0.2)]).unwrap();
        let _other = append_run(&path, "other", "user-7", &[step("elsewhere", 0.4)]).unwrap();
        let mut extra = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        writeln!(
            extra,
            "{}",
            json!({"run_id": earlier, "owner": "fixture", "tier": "read", "tool": "file_read", "gate": "not_required"})
        )
        .unwrap();
        writeln!(
            extra,
            "{}",
            json!({"run_id": earlier, "check": "confidence_low", "node": "classify", "finding": "safe_default"})
        )
        .unwrap();

        let baseline = load_baseline(&path, "ticket", &current).unwrap();
        assert!(baseline.missing_signal(&"classify".into()).is_none());
        assert_eq!(baseline.runs().len(), 1);
        assert_eq!(baseline.runs()[0].run_id, earlier);
        assert_eq!(baseline.runs()[0].events.len(), 1);
        assert_eq!(baseline.runs()[0].events[0].node, "classify".into());
        assert_eq!(baseline.runs()[0].events[0].confidence, Some(0.9));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_empty_baseline_signals_and_has_no_runs() {
        let path = log_path("only-current");
        let current = append_run(&path, "ticket", "user-7", &[step("classify", 0.9)]).unwrap();
        let baseline = load_baseline(&path, "ticket", &current).unwrap();
        assert!(baseline.runs().is_empty());
        let signal = baseline.missing_signal(&"classify".into()).unwrap();
        assert_eq!(signal.check, "baseline_missing");
        assert_eq!(signal.node, "classify".into());
        assert_eq!(signal.finding, "no prior runs");

        let missing = log_path("missing");
        let baseline = load_baseline(&missing, "ticket", "absent").unwrap();
        assert!(baseline.runs().is_empty());
        assert_eq!(
            baseline.missing_signal(&"classify".into()).unwrap().check,
            "baseline_missing"
        );
        std::fs::remove_file(&path).unwrap();
    }
}
