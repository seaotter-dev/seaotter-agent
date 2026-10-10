use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::decision::{Answer, DecisionModel, DecisionRequest};
use crate::effect::{Capability, Effect};
use crate::error::HarnessError;
use crate::generate::{GenOutput, GenRequest, GenerativeModel};
use crate::node::{Gate, Node, NodeId};
use crate::policy::{ConfidenceAction, ConfidencePolicy};
use crate::state::State;
use crate::validate::validate_output;
use crate::workflow::{Next, Outcome, Workflow};

const DEFAULT_MAX_STEPS: usize = 1000;

type Answers = BTreeMap<String, Answer>;
type Decisions = BTreeMap<NodeId, Answers>;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The transition returned `Next::End`.
    Completed,
    /// A low-confidence decision took an edge with no registered handler.
    Halted(ConfidenceAction),
}

/// The result of a finished run.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub state: State,
    /// Every node visited, in order.
    pub path: Vec<NodeId>,
    pub status: RunStatus,
}

/// Configures a [`Machine`]. No capability is granted unless `grant` is called.
pub struct MachineBuilder {
    decision: Option<Box<dyn DecisionModel>>,
    stronger_decision: Option<Box<dyn DecisionModel>>,
    generative: Option<Box<dyn GenerativeModel>>,
    effects: BTreeMap<String, Box<dyn Effect>>,
    granted: BTreeSet<Capability>,
    max_steps: usize,
}

impl MachineBuilder {
    pub fn decision(mut self, model: impl DecisionModel + 'static) -> Self {
        self.decision = Some(Box::new(model));
        self
    }

    /// The model used for the second call of a `RetryStronger` policy.
    pub fn stronger_decision(mut self, model: impl DecisionModel + 'static) -> Self {
        self.stronger_decision = Some(Box::new(model));
        self
    }

    pub fn generative(mut self, model: impl GenerativeModel + 'static) -> Self {
        self.generative = Some(Box::new(model));
        self
    }

    /// Registers an effect under its `name()`, replacing any with the same name.
    pub fn effect(mut self, effect: impl Effect + 'static) -> Self {
        self.effects
            .insert(effect.name().to_owned(), Box::new(effect));
        self
    }

    pub fn grant(mut self, capability: Capability) -> Self {
        self.granted.insert(capability);
        self
    }

    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn build(self) -> Machine {
        Machine {
            decision: self.decision,
            stronger_decision: self.stronger_decision,
            generative: self.generative,
            effects: self.effects,
            granted: self.granted,
            max_steps: self.max_steps,
        }
    }
}

/// Runs a [`Workflow`]. The next node comes from the workflow's transition
/// function, or from its confidence handlers when a decision is low
/// confidence; model output is stored as data, never followed.
pub struct Machine {
    decision: Option<Box<dyn DecisionModel>>,
    stronger_decision: Option<Box<dyn DecisionModel>>,
    generative: Option<Box<dyn GenerativeModel>>,
    effects: BTreeMap<String, Box<dyn Effect>>,
    granted: BTreeSet<Capability>,
    max_steps: usize,
}

impl Machine {
    pub fn builder() -> MachineBuilder {
        MachineBuilder {
            decision: None,
            stronger_decision: None,
            generative: None,
            effects: BTreeMap::new(),
            granted: BTreeSet::new(),
            max_steps: DEFAULT_MAX_STEPS,
        }
    }

    /// Runs `wf` from its start node until the transition returns `Next::End`
    /// or a low-confidence edge has no handler.
    ///
    /// Decide, Generate, and Effect outputs are written to `state[node id]`.
    pub async fn run(&self, wf: &Workflow, mut state: State) -> Result<Run, HarnessError> {
        let mut decisions = Decisions::new();
        let mut path = Vec::new();
        let mut current = wf.start().clone();
        loop {
            if path.len() >= self.max_steps {
                return Err(HarnessError::StepLimit(self.max_steps));
            }
            let node = wf
                .node(&current)
                .ok_or_else(|| HarnessError::Workflow(format!("unknown node `{current}`")))?;
            path.push(current.clone());

            let (outcome, low_edge) = self.step(wf, node, &mut state, &decisions).await?;
            match &outcome {
                Outcome::Decided(answers) => {
                    let value = serde_json::to_value(answers)
                        .map_err(|e| HarnessError::Provider(e.to_string()))?;
                    state.insert(current.0.clone(), value);
                    decisions.insert(current.clone(), answers.clone());
                }
                Outcome::Generated(output) => {
                    let value = match output {
                        GenOutput::Json(v) => v.clone(),
                        GenOutput::Text(s) => Value::String(s.clone()),
                    };
                    state.insert(current.0.clone(), value);
                }
                Outcome::Effected(value) => {
                    state.insert(current.0.clone(), value.clone());
                }
                Outcome::Done | Outcome::Skipped => {}
            }

            if let Some(action) = low_edge {
                match wf.handler(action) {
                    Some(handler) => {
                        current = handler.clone();
                        continue;
                    }
                    None => {
                        return Ok(Run {
                            state,
                            path,
                            status: RunStatus::Halted(action),
                        });
                    }
                }
            }

            match (wf.transition())(&current, &outcome, &state) {
                Next::Goto(next) => current = next,
                Next::End => {
                    return Ok(Run {
                        state,
                        path,
                        status: RunStatus::Completed,
                    });
                }
            }
        }
    }

    /// Runs one node. The second value is the low-confidence edge a `Decide`
    /// node must take instead of the transition, if any.
    async fn step(
        &self,
        wf: &Workflow,
        node: &Node,
        state: &mut State,
        decisions: &Decisions,
    ) -> Result<(Outcome, Option<ConfidenceAction>), HarnessError> {
        let outcome = match node {
            Node::Deterministic { step, .. } => {
                step(state)?;
                Outcome::Done
            }
            Node::Decide {
                reads,
                questions,
                policy,
                ..
            } => {
                let req = DecisionRequest {
                    state: slice(state, reads),
                    questions: questions.clone(),
                };
                let (answers, low_edge) = self.decide(req, policy).await?;
                return Ok((Outcome::Decided(answers), low_edge));
            }
            Node::Generate {
                task,
                reads,
                output,
                max_tokens,
                gate,
                ..
            } => {
                if let Some(gate) = gate
                    && !gate_open(gate, wf, decisions)
                {
                    return Ok((Outcome::Skipped, None));
                }
                let model = self.generative.as_deref().ok_or_else(|| {
                    HarnessError::Workflow("no generative model configured".into())
                })?;
                let req = GenRequest {
                    task: task.clone(),
                    context: Value::Object(slice(state, reads).into_iter().collect()),
                    output: output.clone(),
                    max_tokens: *max_tokens,
                };
                let resp = model.complete(req).await?;
                validate_output(output, &resp.output)?;
                Outcome::Generated(resp.output)
            }
            Node::Effect { name, reads, .. } => {
                let effect = self
                    .effects
                    .get(name)
                    .ok_or_else(|| HarnessError::Workflow(format!("unknown effect `{name}`")))?;
                let capability = effect.capability();
                if !self.granted.contains(&capability) {
                    return Err(HarnessError::CapabilityDenied(capability));
                }
                let input = Value::Object(slice(state, reads).into_iter().collect());
                Outcome::Effected(effect.run(input).await?)
            }
        };
        Ok((outcome, None))
    }

    /// Calls the decision model and applies `policy`. Returns the final
    /// answers and the low-confidence edge to take, if any.
    ///
    /// `RetryStronger` makes one more call to the stronger model with the same
    /// request; if that is still low confidence, the edge is `Escalate`.
    async fn decide(
        &self,
        req: DecisionRequest,
        policy: &ConfidencePolicy,
    ) -> Result<(Answers, Option<ConfidenceAction>), HarnessError> {
        let model = self
            .decision
            .as_deref()
            .ok_or_else(|| HarnessError::Workflow("no decision model configured".into()))?;
        let retry_req = (policy.on_low == ConfidenceAction::RetryStronger).then(|| req.clone());
        let answers = model.evaluate(req).await?.answers;
        if confidence(&answers) >= policy.min_confidence {
            return Ok((answers, None));
        }
        match (policy.on_low, retry_req) {
            (ConfidenceAction::Continue, _) => Ok((answers, None)),
            (ConfidenceAction::RetryStronger, Some(req)) => {
                let stronger = self.stronger_decision.as_deref().ok_or_else(|| {
                    HarnessError::Workflow("no stronger decision model configured".into())
                })?;
                let answers = stronger.evaluate(req).await?.answers;
                if confidence(&answers) >= policy.min_confidence {
                    Ok((answers, None))
                } else {
                    Ok((answers, Some(ConfidenceAction::Escalate)))
                }
            }
            (action, _) => Ok((answers, Some(action))),
        }
    }
}

/// The lowest confidence across a decision's answers. No answers, or a NaN
/// confidence, counts as 0.0.
fn confidence(answers: &Answers) -> f64 {
    answers
        .values()
        .map(|a| {
            if a.confidence.is_nan() {
                0.0
            } else {
                a.confidence
            }
        })
        .reduce(f64::min)
        .unwrap_or(0.0)
}

/// The subset of `state` named by `reads`. Missing keys are left out.
fn slice(state: &State, reads: &[String]) -> State {
    reads
        .iter()
        .filter_map(|key| state.get(key).map(|v| (key.clone(), v.clone())))
        .collect()
}

/// Open only if the gate's Decide node has run, its top answer to `question`
/// is `allow`, and that answer meets the node's `min_confidence`.
fn gate_open(gate: &Gate, wf: &Workflow, decisions: &Decisions) -> bool {
    let Some(Node::Decide { policy, .. }) = wf.node(&gate.decide) else {
        return false;
    };
    decisions
        .get(&gate.decide)
        .and_then(|answers| answers.get(&gate.question))
        .is_some_and(|answer| {
            answer.top() == Some(gate.allow.as_str()) && answer.confidence >= policy.min_confidence
        })
}
