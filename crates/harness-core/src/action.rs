use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::decision::{DecisionModel, DecisionRequest, Question};
use crate::error::HarnessError;
use crate::generate::{GenOutput, GenRequest, GenerativeModel, JsonSchema, OutputSpec};
use crate::machine::DEFAULT_MAX_STEPS;
use crate::validate::validate_output;

const MIN_CONFIDENCE: f64 = 0.7;
const MAX_TOKENS: u32 = 1024;

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

/// One recorded action. `write` is set only for reply and edit.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnStep {
    pub action: Action,
    pub write: Option<GenOutput>,
}

/// Asks `model` what to do with one user message until the loop ends.
///
/// The decision request carries `message` and `system` only. Reply and edit
/// are the only actions that call `writer`. A shell answer below 0.7 is not
/// recorded. This function runs no effect.
pub async fn choose(
    model: &dyn DecisionModel,
    writer: &dyn GenerativeModel,
    message: &str,
    system: &str,
) -> Result<Vec<TurnStep>, HarnessError> {
    let mut steps = Vec::new();
    loop {
        if steps.len() >= DEFAULT_MAX_STEPS {
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
            return Ok(steps);
        }
        let write = write(writer, action, message, system).await?;
        steps.push(TurnStep { action, write });
        if matches!(action, Action::Reply | Action::Stop) {
            return Ok(steps);
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

async fn write(
    writer: &dyn GenerativeModel,
    action: Action,
    message: &str,
    system: &str,
) -> Result<Option<GenOutput>, HarnessError> {
    let Some(spec) = output_spec(action) else {
        return Ok(None);
    };
    let req = GenRequest {
        task: match action {
            Action::Reply => "Reply to the user.".into(),
            Action::Edit => "Return a patch.".into(),
            _ => String::new(),
        },
        context: json!({
            "message": message,
            "system": system,
        }),
        output: spec.clone(),
        max_tokens: MAX_TOKENS,
    };
    let resp = writer.complete(req).await?;
    validate_output(&spec, &resp.output)?;
    Ok(Some(resp.output))
}

fn output_spec(action: Action) -> Option<OutputSpec> {
    match action {
        Action::Reply => Some(OutputSpec::FreeText),
        Action::Edit => Some(OutputSpec::Schema(JsonSchema(patch_schema()))),
        Action::Read | Action::Search | Action::Stop | Action::Shell => None,
    }
}

fn patch_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "patch"],
        "properties": {
            "path": { "type": "string" },
            "patch": { "type": "string" }
        }
    })
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
