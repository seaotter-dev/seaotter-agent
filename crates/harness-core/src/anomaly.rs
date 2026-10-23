//! Anomaly findings for one finished run against a baseline.
//!
//! [`anomaly`] returns one signal per finding. It does not change the step list.

use serde_json::Value;

use crate::drift::Baseline;
use crate::event::{Event, EventKind, Signal};
use crate::node::NodeId;

/// One signal per unseen branch, confidence below the whole baseline, or recorded effect error.
///
/// An empty baseline returns nothing. A node sequence that appears in one prior
/// run is not an `anomaly_branch` finding.
pub fn anomaly(baseline: &Baseline, current: &[Event]) -> Vec<Signal> {
    if baseline.runs().is_empty() {
        return Vec::new();
    }
    let mut signals = Vec::new();
    let current_nodes: Vec<NodeId> = current.iter().map(|event| event.node.clone()).collect();
    let recorded = baseline
        .runs()
        .iter()
        .any(|run| node_sequence(&run.events) == current_nodes);
    if !recorded && let Some(node) = branch_node(&current_nodes, baseline) {
        signals.push(Signal {
            check: "anomaly_branch".into(),
            node,
            finding: "branch".into(),
        });
    }
    for event in current {
        let Some(confidence) = event.confidence else {
            continue;
        };
        let samples: Vec<f64> = baseline
            .runs()
            .iter()
            .flat_map(|run| run.events.iter())
            .filter(|prior| prior.node == event.node)
            .filter_map(|prior| prior.confidence)
            .collect();
        if samples.is_empty() {
            continue;
        }
        if samples.iter().all(|sample| confidence < *sample) {
            signals.push(Signal {
                check: "anomaly_confidence".into(),
                node: event.node.clone(),
                finding: "confidence".into(),
            });
        }
    }
    for event in current {
        if event.kind == EventKind::Effect && effect_error_message(&event.output).is_some() {
            signals.push(Signal {
                check: "anomaly_effect".into(),
                node: event.node.clone(),
                finding: "effect".into(),
            });
        }
    }
    signals
}

fn node_sequence(events: &[Event]) -> Vec<NodeId> {
    events.iter().map(|event| event.node.clone()).collect()
}

fn branch_node(current: &[NodeId], baseline: &Baseline) -> Option<NodeId> {
    for (index, node) in current.iter().enumerate() {
        let seen = baseline.runs().iter().any(|run| {
            run.events
                .get(index)
                .is_some_and(|event| &event.node == node)
        });
        if !seen {
            return Some(node.clone());
        }
    }
    current.last().cloned()
}

fn effect_error_message(output: &Value) -> Option<&str> {
    output.get("error").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drift::load_baseline;
    use crate::event::{EventLog, Usage, append_run, read_run};
    use serde_json::{Value, json};

    fn event(node: &str, kind: EventKind, confidence: Option<f64>, output: Value) -> Event {
        Event {
            node: node.into(),
            name: node.into(),
            kind,
            input_hash: "abc".into(),
            output,
            confidence,
            usage: Usage::default(),
        }
    }

    fn decide(node: &str, confidence: f64, output: Value) -> Event {
        event(node, EventKind::Decide, Some(confidence), output)
    }

    fn log_path(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("seaotter-anomaly-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
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
    fn a_recorded_branch_has_no_anomaly() {
        let recorded_effect = event("read", EventKind::Effect, None, json!({"body": "ok"}));
        let (baseline, path) = prior_runs(
            "recorded",
            &[
                vec![decide("billing", 0.9, json!({"route": "billing"}))],
                vec![
                    decide("technical", 0.9, json!({"route": "billing"})),
                    recorded_effect.clone(),
                ],
            ],
        );
        let signals = anomaly(
            &baseline,
            &[
                decide("technical", 0.9, json!({"route": "billing"})),
                recorded_effect,
            ],
        );
        assert!(signals.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_unseen_branch_a_lower_confidence_and_an_effect_error_each_signal() {
        let prior = vec![
            decide("classify", 0.9, json!({"route": "billing"})),
            decide("billing", 0.8, json!({"done": true})),
        ];
        let (baseline, path) = prior_runs("anomalous", &[prior.clone(), prior]);
        let current = vec![
            decide("classify", 0.4, json!({"route": "billing"})),
            decide("queue", 0.8, json!({"done": true})),
            event(
                "read",
                EventKind::Effect,
                None,
                json!({"error": "missing file"}),
            ),
        ];
        let signals = anomaly(&baseline, &current);
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
                ("anomaly_branch", "queue", "branch"),
                ("anomaly_confidence", "classify", "confidence"),
                ("anomaly_effect", "read", "effect"),
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
        let recorded = log_path("anomalous-log");
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
    fn a_confidence_equal_to_the_lowest_baseline_is_not_an_anomaly() {
        let (baseline, path) = prior_runs(
            "lowest",
            &[
                vec![decide("classify", 0.9, json!({"route": "billing"}))],
                vec![decide("classify", 0.8, json!({"route": "billing"}))],
            ],
        );
        let current = [decide("classify", 0.8, json!({"route": "billing"}))];
        let signals = anomaly(&baseline, &current);
        assert!(
            signals
                .iter()
                .all(|signal| signal.check != "anomaly_confidence")
        );
        assert!(signals.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_empty_baseline_has_no_anomaly_findings() {
        let missing = log_path("missing");
        let baseline = load_baseline(&missing, "ticket", "absent").unwrap();
        let current = [decide("classify", 0.1, json!({"route": "billing"}))];
        assert!(anomaly(&baseline, &current).is_empty());
        assert_eq!(
            baseline.missing_signal(&"classify".into()).unwrap().check,
            "baseline_missing"
        );
    }
}
