use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::decision::{Answer, DecisionModel, DecisionRequest};
use crate::effect::{Capability, Effect};
use crate::error::HarnessError;
use crate::generate::{GenOutput, GenRequest, GenerativeModel};
use crate::node::{Gate, Node, NodeId};
use crate::state::State;
use crate::validate::validate_output;
use crate::workflow::{Next, Outcome, Workflow};

const DEFAULT_MAX_STEPS: usize = 1000;

type Decisions = BTreeMap<NodeId, BTreeMap<String, Answer>>;

/// The result of a completed run.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub state: State,
    /// Every node visited, in order.
    pub path: Vec<NodeId>,
}

/// Configures a [`Machine`]. No capability is granted unless `grant` is called.
pub struct MachineBuilder {
    decision: Option<Box<dyn DecisionModel>>,
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
            generative: self.generative,
            effects: self.effects,
            granted: self.granted,
            max_steps: self.max_steps,
        }
    }
}

/// Runs a [`Workflow`]. The next node always comes from the workflow's
/// transition function; model output is stored as data, never followed.
pub struct Machine {
    decision: Option<Box<dyn DecisionModel>>,
    generative: Option<Box<dyn GenerativeModel>>,
    effects: BTreeMap<String, Box<dyn Effect>>,
    granted: BTreeSet<Capability>,
    max_steps: usize,
}

impl Machine {
    pub fn builder() -> MachineBuilder {
        MachineBuilder {
            decision: None,
            generative: None,
            effects: BTreeMap::new(),
            granted: BTreeSet::new(),
            max_steps: DEFAULT_MAX_STEPS,
        }
    }

    /// Runs `wf` from its start node until the transition returns `Next::End`.
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

            let outcome = self.step(wf, node, &mut state, &decisions).await?;
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

            match (wf.transition())(&current, &outcome, &state) {
                Next::Goto(next) => current = next,
                Next::End => return Ok(Run { state, path }),
            }
        }
    }

    async fn step(
        &self,
        wf: &Workflow,
        node: &Node,
        state: &mut State,
        decisions: &Decisions,
    ) -> Result<Outcome, HarnessError> {
        match node {
            Node::Deterministic { step, .. } => {
                step(state)?;
                Ok(Outcome::Done)
            }
            Node::Decide {
                reads, questions, ..
            } => {
                let model = self
                    .decision
                    .as_deref()
                    .ok_or_else(|| HarnessError::Workflow("no decision model configured".into()))?;
                let req = DecisionRequest {
                    state: slice(state, reads),
                    questions: questions.clone(),
                };
                Ok(Outcome::Decided(model.evaluate(req).await?.answers))
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
                    return Ok(Outcome::Skipped);
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
                Ok(Outcome::Generated(resp.output))
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
                Ok(Outcome::Effected(effect.run(input).await?))
            }
        }
    }
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
