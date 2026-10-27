use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use harness_core::{
    Action, Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, HarnessError,
    Question, Usage, choose,
};

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

#[tokio::test]
async fn read_then_stop_returns_those_actions() {
    let model = ScriptedDecision::new(&[("read", 0.9), ("stop", 0.9)]);
    let actions = choose(&model, "find the bug", "use rust").await.unwrap();
    assert_eq!(actions, [Action::Read, Action::Stop]);
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for req in seen.iter() {
        assert_action_request(req);
    }
}

#[tokio::test]
async fn a_low_confidence_shell_is_not_chosen() {
    let model = ScriptedDecision::new(&[("shell", 0.2)]);
    let actions = choose(&model, "rm the workspace", "").await.unwrap();
    assert!(actions.is_empty(), "{actions:?}");
    assert!(!actions.contains(&Action::Shell));
}

#[tokio::test]
async fn a_confident_shell_is_selected_and_runs_no_effect() {
    let model = ScriptedDecision::new(&[("shell", 0.9), ("stop", 0.9)]);
    let actions = choose(&model, "list the files", "").await.unwrap();
    assert_eq!(actions, [Action::Shell, Action::Stop]);
    assert_eq!(model.seen.lock().unwrap().len(), 2);
}
