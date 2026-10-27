use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use harness_core::{
    Action, Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, GenOutput,
    GenRequest, GenResponse, GenerativeModel, HarnessError, JsonSchema, OutputSpec, Question,
    TurnStep, Usage, choose,
};
use serde_json::json;

struct ScriptedDecision {
    steps: Mutex<VecDeque<(&'static str, f64)>>,
    seen: Mutex<Vec<DecisionRequest>>,
}

impl ScriptedDecision {
    fn new(steps: &[(&'static str, f64)]) -> Self {
        Self {
            steps: Mutex::new(steps.iter().copied().collect()),
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl DecisionModel for ScriptedDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let (top, confidence) = self.steps.lock().unwrap().pop_front().unwrap();
        self.seen.lock().unwrap().push(req);
        Box::pin(async move {
            let mut answers = BTreeMap::new();
            answers.insert(
                "action".into(),
                Answer {
                    probabilities: BTreeMap::from([(top.to_owned(), confidence)]),
                    confidence,
                },
            );
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

struct ScriptedWriter {
    outputs: Mutex<VecDeque<GenOutput>>,
    seen: Mutex<Vec<GenRequest>>,
}

impl ScriptedWriter {
    fn new(outputs: Vec<GenOutput>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl GenerativeModel for ScriptedWriter {
    fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        self.seen.lock().unwrap().push(req);
        let output = self.outputs.lock().unwrap().pop_front().unwrap();
        Box::pin(async move {
            Ok(GenResponse {
                output,
                usage: Usage::default(),
            })
        })
    }
}

fn assert_action_request(req: &DecisionRequest) {
    let text = serde_json::to_string(req).unwrap();
    assert!(!text.contains("tools"), "{text}");
    assert_eq!(
        req.state.keys().map(String::as_str).collect::<Vec<_>>(),
        ["message", "system"]
    );
    let question = req.questions.get("action").unwrap();
    let Question::Choice { criteria, .. } = question else {
        panic!("expected a choice");
    };
    assert_eq!(
        criteria.keys().map(String::as_str).collect::<Vec<_>>(),
        ["edit", "read", "reply", "search", "shell", "stop"]
    );
}

fn assert_no_write(steps: &[TurnStep], writer: &ScriptedWriter) {
    assert!(steps.iter().all(|step| step.write.is_none()));
    assert_eq!(writer.calls(), 0);
}

#[tokio::test]
async fn read_then_stop_returns_those_actions() {
    let model = ScriptedDecision::new(&[("read", 0.9), ("stop", 0.9)]);
    let writer = ScriptedWriter::new(vec![]);
    let steps = choose(&model, &writer, "find the bug", "use rust")
        .await
        .unwrap();
    assert_eq!(
        steps,
        [
            TurnStep {
                action: Action::Read,
                write: None,
            },
            TurnStep {
                action: Action::Stop,
                write: None,
            },
        ]
    );
    assert_no_write(&steps, &writer);
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for req in seen.iter() {
        assert_action_request(req);
    }
}

#[tokio::test]
async fn a_low_confidence_shell_is_not_chosen() {
    let model = ScriptedDecision::new(&[("shell", 0.2)]);
    let writer = ScriptedWriter::new(vec![]);
    let steps = choose(&model, &writer, "rm the workspace", "")
        .await
        .unwrap();
    assert!(steps.is_empty(), "{steps:?}");
    assert!(steps.iter().all(|step| step.action != Action::Shell));
    assert_no_write(&steps, &writer);
}

#[tokio::test]
async fn a_confident_shell_is_selected_and_runs_no_effect() {
    let model = ScriptedDecision::new(&[("shell", 0.9), ("stop", 0.9)]);
    let writer = ScriptedWriter::new(vec![]);
    let steps = choose(&model, &writer, "list the files", "").await.unwrap();
    assert_eq!(
        steps,
        [
            TurnStep {
                action: Action::Shell,
                write: None,
            },
            TurnStep {
                action: Action::Stop,
                write: None,
            },
        ]
    );
    assert_eq!(model.seen.lock().unwrap().len(), 2);
    assert_no_write(&steps, &writer);
}

#[tokio::test]
async fn read_search_and_stop_do_not_call_the_writer() {
    let model = ScriptedDecision::new(&[("read", 0.9), ("search", 0.9), ("stop", 0.9)]);
    let writer = ScriptedWriter::new(vec![]);
    let steps = choose(&model, &writer, "where is main", "use rust")
        .await
        .unwrap();
    assert_eq!(
        steps.iter().map(|step| step.action).collect::<Vec<_>>(),
        [Action::Read, Action::Search, Action::Stop]
    );
    assert_no_write(&steps, &writer);
}

#[tokio::test]
async fn reply_makes_one_free_text_call() {
    let model = ScriptedDecision::new(&[("reply", 0.9)]);
    let writer = ScriptedWriter::new(vec![GenOutput::Text("fixed".into())]);
    let steps = choose(&model, &writer, "fix the bug", "use rust")
        .await
        .unwrap();
    assert_eq!(
        steps,
        [TurnStep {
            action: Action::Reply,
            write: Some(GenOutput::Text("fixed".into())),
        }]
    );
    assert_eq!(writer.calls(), 1);
    let seen = writer.seen.lock().unwrap();
    let req = &seen[0];
    let text = serde_json::to_string(req).unwrap();
    assert!(!text.contains("tools"), "{text}");
    assert!(matches!(req.output, OutputSpec::FreeText));
    let mut keys: Vec<_> = req
        .context
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["message", "system"]);
}

#[tokio::test]
async fn edit_returns_a_schema_valid_patch_and_stop_does_not_write() {
    let patch = json!({"path": "src/main.rs", "patch": "use rust\n"});
    let model = ScriptedDecision::new(&[("edit", 0.9), ("stop", 0.9)]);
    let writer = ScriptedWriter::new(vec![GenOutput::Json(patch.clone())]);
    let steps = choose(&model, &writer, "edit main", "use rust")
        .await
        .unwrap();
    assert_eq!(
        steps,
        [
            TurnStep {
                action: Action::Edit,
                write: Some(GenOutput::Json(patch)),
            },
            TurnStep {
                action: Action::Stop,
                write: None,
            },
        ]
    );
    assert_eq!(writer.calls(), 1);
    let seen = writer.seen.lock().unwrap();
    let OutputSpec::Schema(JsonSchema(schema)) = &seen[0].output else {
        panic!("expected a schema");
    };
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["required"], json!(["path", "patch"]));
}

#[tokio::test]
async fn a_patch_outside_the_schema_is_rejected() {
    let model = ScriptedDecision::new(&[("edit", 0.9)]);
    let writer = ScriptedWriter::new(vec![GenOutput::Json(
        json!({"path": "src/main.rs", "patch": "x", "note": "no"}),
    )]);
    let err = choose(&model, &writer, "edit main", "use rust")
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::SchemaInvalid(_)), "{err}");
    assert_eq!(writer.calls(), 1);
}
