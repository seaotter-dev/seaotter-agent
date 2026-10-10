use std::collections::BTreeMap;

use serde_json::Value;

use crate::decision::Answer;
use crate::error::HarnessError;
use crate::generate::GenOutput;
use crate::node::{Node, NodeId};
use crate::policy::ConfidenceAction;
use crate::state::State;

/// What a node produced. Data only; it never names the next node.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A `Deterministic` step finished.
    Done,
    /// A `Decide` node's answers, keyed by question.
    Decided(BTreeMap<String, Answer>),
    /// A `Generate` node's output, already validated against its spec.
    Generated(GenOutput),
    /// A `Generate` node whose gate denied it. The model was not called.
    Skipped,
    /// An `Effect` node's result.
    Effected(Value),
}

/// The step after a node, chosen by the workflow's transition function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Goto(NodeId),
    End,
}

/// Picks the next node with a Rust match over the node just run and its outcome.
pub type Transition = fn(&NodeId, &Outcome, &State) -> Next;

/// An explicit node graph: Rust-built nodes plus a Rust transition function.
///
/// Low-confidence decisions bypass the transition and go to the handler
/// registered with [`Workflow::on_low`], or halt the run if there is none.
#[derive(Debug, Clone)]
pub struct Workflow {
    start: NodeId,
    nodes: BTreeMap<NodeId, Node>,
    transition: Transition,
    handlers: BTreeMap<ConfidenceAction, NodeId>,
}

impl Workflow {
    /// Rejects duplicate node ids, a missing `start`, and gates that do not
    /// point at a `Decide` node.
    pub fn new(
        start: NodeId,
        nodes: Vec<Node>,
        transition: Transition,
    ) -> Result<Self, HarnessError> {
        let mut map = BTreeMap::new();
        for node in nodes {
            let id = node.id().clone();
            if map.insert(id.clone(), node).is_some() {
                return Err(HarnessError::Workflow(format!("duplicate node id `{id}`")));
            }
        }
        if !map.contains_key(&start) {
            return Err(HarnessError::Workflow(format!(
                "start node `{start}` is not defined"
            )));
        }
        for node in map.values() {
            if let Node::Generate {
                id,
                gate: Some(gate),
                ..
            } = node
                && !matches!(map.get(&gate.decide), Some(Node::Decide { .. }))
            {
                return Err(HarnessError::Workflow(format!(
                    "gate on `{id}` must point at a Decide node, got `{}`",
                    gate.decide
                )));
            }
        }
        Ok(Self {
            start,
            nodes: map,
            transition,
            handlers: BTreeMap::new(),
        })
    }

    /// Routes low-confidence decisions that resolve to `action` to `handler`.
    ///
    /// Only `Escalate`, `SafeDefault`, and `Human` are edges; `Continue` and
    /// `RetryStronger` are rejected, as is an undefined handler node.
    pub fn on_low(
        mut self,
        action: ConfidenceAction,
        handler: NodeId,
    ) -> Result<Self, HarnessError> {
        if matches!(
            action,
            ConfidenceAction::Continue | ConfidenceAction::RetryStronger
        ) {
            return Err(HarnessError::Workflow(format!(
                "{action:?} is not a low-confidence edge"
            )));
        }
        if !self.nodes.contains_key(&handler) {
            return Err(HarnessError::Workflow(format!(
                "handler node `{handler}` is not defined"
            )));
        }
        self.handlers.insert(action, handler);
        Ok(self)
    }

    pub fn handler(&self, action: ConfidenceAction) -> Option<&NodeId> {
        self.handlers.get(&action)
    }

    pub fn start(&self) -> &NodeId {
        &self.start
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn transition(&self) -> Transition {
        self.transition
    }
}
