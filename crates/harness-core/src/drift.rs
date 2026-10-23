//! Prior runs of one workflow, read from the append-only log.
//!
//! [`Baseline::drift`] compares a new run with that history and returns one
//! signal per finding. It does not change the step list.

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

    /// One signal per branch, confidence, or output finding.
    ///
    /// An empty baseline returns nothing. A node with no baseline confidence
    /// is not a confidence finding.
    pub fn drift(&self, current: &[Event], thresholds: &DriftThresholds) -> Vec<Signal> {
        if self.runs.is_empty() {
            return Vec::new();
        }
        let mut signals = Vec::new();
        let current_nodes: Vec<NodeId> = current.iter().map(|event| event.node.clone()).collect();
        let matched = self
            .runs
            .iter()
            .filter(|run| node_sequence(&run.events) == current_nodes)
            .count();
        let share = matched as f64 / self.runs.len() as f64;
        if share < thresholds.branch_match
            && let Some(node) = branch_node(&current_nodes, &majority_sequence(&self.runs))
        {
            signals.push(Signal {
                check: "drift_branch".into(),
                node,
                finding: "branch".into(),
            });
        }
        for event in current {
            let Some(confidence) = event.confidence else {
                continue;
            };
            let mut samples: Vec<f64> = self
                .runs
                .iter()
                .flat_map(|run| run.events.iter())
                .filter(|prior| prior.node == event.node)
                .filter_map(|prior| prior.confidence)
                .collect();
            if samples.is_empty() {
                continue;
            }
            let median = median(&mut samples);
            if (confidence - median).abs() > thresholds.confidence_delta {
                signals.push(Signal {
                    check: "drift_confidence".into(),
                    node: event.node.clone(),
                    finding: "confidence".into(),
                });
            }
        }
        for (index, event) in current.iter().enumerate() {
            let mut compared = 0usize;
            let mut matched = 0usize;
            for run in &self.runs {
                let Some(prior) = run.events.get(index) else {
                    continue;
                };
                compared += 1;
                if prior.output == event.output {
                    matched += 1;
                }
            }
            if compared > 0 && (matched as f64) / (compared as f64) < thresholds.output_match {
                signals.push(Signal {
                    check: "drift_output".into(),
                    node: event.node.clone(),
                    finding: "output".into(),
                });
            }
        }
        signals
    }
}

/// Caller-supplied shares and gaps for [`Baseline::drift`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftThresholds {
    /// Minimum share of baseline runs that must use the new node sequence.
    pub branch_match: f64,
    /// Maximum absolute gap from the median baseline confidence for a node.
    pub confidence_delta: f64,
    /// Minimum share of baseline events at an index that must share the new output.
    pub output_match: f64,
}

fn node_sequence(events: &[Event]) -> Vec<NodeId> {
    events.iter().map(|event| event.node.clone()).collect()
}

fn majority_sequence(runs: &[PriorRun]) -> Vec<NodeId> {
    let mut counts: Vec<(Vec<NodeId>, usize)> = Vec::new();
    for run in runs {
        let sequence = node_sequence(&run.events);
        if let Some((_, count)) = counts.iter_mut().find(|(seen, _)| seen == &sequence) {
            *count += 1;
        } else {
            counts.push((sequence, 1));
        }
    }
    let mut best: Option<(Vec<NodeId>, usize)> = None;
    for (sequence, count) in counts {
        if best
            .as_ref()
            .is_none_or(|(_, best_count)| count > *best_count)
        {
            best = Some((sequence, count));
        }
    }
    best.map(|(sequence, _)| sequence).unwrap_or_default()
}

fn branch_node(current: &[NodeId], reference: &[NodeId]) -> Option<NodeId> {
    for (index, node) in current.iter().enumerate() {
        if reference.get(index) != Some(node) {
            return Some(node.clone());
        }
    }
    current
        .last()
        .cloned()
        .or_else(|| reference.first().cloned())
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|left, right| left.total_cmp(right));
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
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
    use crate::event::{EventKind, EventLog, append_run, read_run};
    use serde_json::{Value, json};

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

    fn event(node: &str, confidence: f64, output: Value) -> Event {
        Event {
            node: node.into(),
            name: node.into(),
            kind: EventKind::Decide,
            input_hash: "abc".into(),
            output,
            confidence: Some(confidence),
            usage: Usage::default(),
        }
    }

    fn strict() -> DriftThresholds {
        DriftThresholds {
            branch_match: 1.0,
            confidence_delta: 0.1,
            output_match: 1.0,
        }
    }

    fn prior_runs(name: &str, runs: &[Vec<Event>]) -> (Baseline, std::path::PathBuf) {
        let path = log_path(name);
        for run in runs {
            append_run(&path, "ticket", "user-7", run).unwrap();
        }
        let baseline = load_baseline(&path, "ticket", "current-run").unwrap();
        (baseline, path)
    }

    #[test]
    fn a_run_inside_the_thresholds_has_no_drift() {
        let prior = vec![event("classify", 0.9, json!({"route": "billing"}))];
        let (baseline, path) = prior_runs("inside", &[prior]);
        let signals = baseline.drift(
            &[event("classify", 0.85, json!({"route": "billing"}))],
            &strict(),
        );
        assert!(signals.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn branch_confidence_and_output_each_append_one_signal() {
        let prior = vec![
            event("classify", 0.9, json!({"route": "billing"})),
            event("billing", 0.8, json!({"done": true})),
        ];
        let (baseline, path) = prior_runs("drifted", &[prior.clone(), prior]);
        let current = vec![
            event("classify", 0.4, json!({"route": "technical"})),
            event("queue", 0.8, json!({"done": true})),
        ];
        let signals = baseline.drift(&current, &strict());
        assert_eq!(
            signals
                .iter()
                .map(|signal| (
                    signal.check.as_str(),
                    signal.node.0.as_str(),
                    signal.finding.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("drift_branch", "queue", "branch"),
                ("drift_confidence", "classify", "confidence"),
                ("drift_output", "classify", "output"),
            ]
        );

        let mut log = EventLog::default();
        for step in &current {
            log.push(step.clone());
        }
        for signal in signals {
            log.push_signal(signal);
        }
        assert_eq!(log.len(), current.len());
        let recorded = log_path("drifted-log");
        let run_id = log.append_to(&recorded, "ticket", "user-7").unwrap();
        assert_eq!(read_run(&recorded, &run_id).unwrap(), current);
        let text = std::fs::read_to_string(&recorded).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|value: &Value| value.get("check").is_some())
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|line| line.get("kind").is_none()));
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&recorded).unwrap();
    }

    #[test]
    fn a_recorded_minority_branch_is_still_drift() {
        let (baseline, path) = prior_runs(
            "minority",
            &[
                vec![event("billing", 0.9, json!({"route": "billing"}))],
                vec![event("technical", 0.9, json!({"route": "billing"}))],
            ],
        );
        let signals = baseline.drift(
            &[event("technical", 0.9, json!({"route": "billing"}))],
            &DriftThresholds {
                branch_match: 1.0,
                confidence_delta: 0.0,
                output_match: 0.5,
            },
        );
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].check, "drift_branch");
        assert_eq!(signals[0].node, "technical".into());
        assert_eq!(signals[0].finding, "branch");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_empty_baseline_has_no_drift_findings() {
        let missing = log_path("missing-drift");
        let baseline = load_baseline(&missing, "ticket", "absent").unwrap();
        assert!(
            baseline
                .drift(&[step("classify", 0.9)], &strict())
                .is_empty()
        );
        assert_eq!(
            baseline.missing_signal(&"classify".into()).unwrap().check,
            "baseline_missing"
        );
    }
}
