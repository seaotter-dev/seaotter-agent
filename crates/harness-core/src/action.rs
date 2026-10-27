use std::collections::BTreeMap;

use crate::decision::{DecisionModel, DecisionRequest, Question};
use crate::error::HarnessError;
use crate::machine::DEFAULT_MAX_STEPS;

const MIN_CONFIDENCE: f64 = 0.7;

/// One action System One can choose. Rust matches on this; the model does not
/// pick the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Search,
    Edit,
    Shell,
    Reply,
    Stop,
}

/// Asks `model` what to do with one user message until the loop ends.
///
/// The request carries `message` and `system` only. A shell answer below 0.7
/// is not recorded. This function runs no effect.
pub async fn choose(
    model: &dyn DecisionModel,
    message: &str,
    system: &str,
) -> Result<Vec<Action>, HarnessError> {
    let mut actions = Vec::new();
    loop {
        if actions.len() >= DEFAULT_MAX_STEPS {
            return Err(HarnessError::StepLimit(DEFAULT_MAX_STEPS));
        }
        let req = request(message, system)?;
        let resp = model.evaluate(req).await?;
        let answer = resp
            .answers
            .get("action")
            .ok_or_else(|| HarnessError::Workflow("missing action answer".into()))?;
        let label = answer
            .top()
            .ok_or_else(|| HarnessError::Workflow("action has no label".into()))?;
        let action = parse_action(label)?;
        if action == Action::Shell && answer.confidence < MIN_CONFIDENCE {
            return Ok(actions);
        }
        actions.push(action);
        if matches!(action, Action::Reply | Action::Stop) {
            return Ok(actions);
        }
    }
}

fn request(message: &str, system: &str) -> Result<DecisionRequest, HarnessError> {
    Ok(DecisionRequest::builder(["message", "system"])
        .set("message", message)?
        .set("system", system)?
        .ask("action", action_question())?
        .build())
}

fn action_question() -> Question {
    Question::Choice {
        instructions: "Choose the next action.".into(),
        criteria: BTreeMap::from([
            ("read".into(), "Read a file in the workspace.".into()),
            ("search".into(), "Search the workspace.".into()),
            ("edit".into(), "Edit a file in the workspace.".into()),
            ("shell".into(), "Run a shell command.".into()),
            ("reply".into(), "Reply to the user.".into()),
            ("stop".into(), "Stop.".into()),
        ]),
    }
}

fn parse_action(label: &str) -> Result<Action, HarnessError> {
    match label {
        "read" => Ok(Action::Read),
        "search" => Ok(Action::Search),
        "edit" => Ok(Action::Edit),
        "shell" => Ok(Action::Shell),
        "reply" => Ok(Action::Reply),
        "stop" => Ok(Action::Stop),
        other => Err(HarnessError::Workflow(format!("unknown action `{other}`"))),
    }
}
