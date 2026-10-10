use serde_json::Value;

use crate::machine::Run;
use crate::node::NodeId;

/// One way two runs disagree. Usage and input hashes are not compared.
#[derive(Debug, Clone, PartialEq)]
pub enum Divergence {
    /// `path[index]` differs. A missing side is a node only the other run visited.
    Branch {
        index: usize,
        left: Option<NodeId>,
        right: Option<NodeId>,
    },
    /// Event `index` is missing on one side, names a different node, or stored
    /// a different output.
    Output {
        index: usize,
        left_node: Option<NodeId>,
        right_node: Option<NodeId>,
        left: Option<Value>,
        right: Option<Value>,
    },
    /// The recorded confidence at event `index` differs, including a side that
    /// has no event.
    Confidence {
        index: usize,
        node: NodeId,
        left: Option<f64>,
        right: Option<f64>,
    },
}

/// Reports branch, output, and confidence differences between two finished runs.
pub fn diff_runs(left: &Run, right: &Run) -> Vec<Divergence> {
    let mut found = Vec::new();
    let path_len = left.path.len().max(right.path.len());
    for index in 0..path_len {
        let left_node = left.path.get(index);
        let right_node = right.path.get(index);
        if left_node != right_node {
            found.push(Divergence::Branch {
                index,
                left: left_node.cloned(),
                right: right_node.cloned(),
            });
        }
    }

    let left_events = left.events.as_slice();
    let right_events = right.events.as_slice();
    let event_len = left_events.len().max(right_events.len());
    for index in 0..event_len {
        let left_event = left_events.get(index);
        let right_event = right_events.get(index);
        let nodes_differ =
            left_event.map(|event| &event.node) != right_event.map(|event| &event.node);
        let outputs_differ =
            left_event.map(|event| &event.output) != right_event.map(|event| &event.output);
        if left_event.is_none() || right_event.is_none() || nodes_differ || outputs_differ {
            found.push(Divergence::Output {
                index,
                left_node: left_event.map(|event| event.node.clone()),
                right_node: right_event.map(|event| event.node.clone()),
                left: left_event.map(|event| event.output.clone()),
                right: right_event.map(|event| event.output.clone()),
            });
        }
        let left_confidence = left_event.and_then(|event| event.confidence);
        let right_confidence = right_event.and_then(|event| event.confidence);
        if left_confidence != right_confidence
            && let Some(node) = left_event.or(right_event).map(|event| event.node.clone())
        {
            found.push(Divergence::Confidence {
                index,
                node,
                left: left_confidence,
                right: right_confidence,
            });
        }
    }
    found
}
