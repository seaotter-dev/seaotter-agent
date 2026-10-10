//! Swapping the generative client does not change `Machine` control flow.

mod common;

use common::serve_once;
use harness_core::{
    GenerativeModel, HarnessError, JsonSchema, Machine, Next, Node, NodeId, Outcome, OutputSpec,
    Run, RunStatus, State, Workflow,
};
use harness_providers::{Anthropic, AnthropicConfig, OpenAiCompatible, OpenAiCompatibleConfig};
use serde_json::{Value, json};

const OPENAI_VALID: &str = include_str!("fixtures/chat_schema_valid.json");
const OPENAI_INVALID: &str = include_str!("fixtures/chat_schema_invalid.json");
const ANTHROPIC_VALID: &str = include_str!("fixtures/messages_schema_valid.json");
const ANTHROPIC_INVALID: &str = include_str!("fixtures/messages_schema_invalid.json");

fn record(state: &mut State) -> Result<(), HarnessError> {
    let priority = state
        .get("draft")
        .and_then(|d| d.get("priority"))
        .cloned()
        .unwrap_or(Value::Null);
    state.insert("recorded_priority".into(), priority);
    Ok(())
}

fn fallback(state: &mut State) -> Result<(), HarnessError> {
    state.insert("fell_back".into(), json!(true));
    Ok(())
}

fn transition(node: &NodeId, outcome: &Outcome, _state: &State) -> Next {
    match (node.0.as_str(), outcome) {
        ("draft", Outcome::Generated(_)) => Next::Goto("record".into()),
        ("draft", _) => Next::Goto("fallback".into()),
        _ => Next::End,
    }
}

fn workflow() -> Workflow {
    let nodes = vec![
        Node::Generate {
            id: "draft".into(),
            task: "Summarize the ticket and set a priority.".into(),
            reads: vec!["ticket".into()],
            output: OutputSpec::Schema(JsonSchema(json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "maxLength": 80},
                    "priority": {"type": "string", "enum": ["low", "normal", "high"]}
                },
                "required": ["summary", "priority"]
            }))),
            max_tokens: 200,
            gate: None,
        },
        Node::Deterministic {
            id: "record".into(),
            step: record,
        },
        Node::Deterministic {
            id: "fallback".into(),
            step: fallback,
        },
    ];
    Workflow::new("draft".into(), nodes, transition).unwrap()
}

fn state() -> State {
    State::from([(
        "ticket".into(),
        json!("My blender arrived broken. I want to return it."),
    )])
}

async fn run_with(model: impl GenerativeModel + 'static) -> Result<Run, HarnessError> {
    Machine::builder()
        .generative(model)
        .build()
        .run(&workflow(), state())
        .await
}

async fn run_openai(fixture: &'static str) -> Result<Run, HarnessError> {
    let (base_url, server) = serve_once("200 OK", fixture).await;
    let config = OpenAiCompatibleConfig::new(base_url, "test-key", "grok-4");
    let result = run_with(OpenAiCompatible::new(config).unwrap()).await;
    server.await.unwrap();
    result
}

async fn run_anthropic(fixture: &'static str) -> Result<Run, HarnessError> {
    let (base_url, server) = serve_once("200 OK", fixture).await;
    let mut config = AnthropicConfig::new("test-key", "claude-sonnet-5-5");
    config.base_url = base_url;
    let result = run_with(Anthropic::new(config).unwrap()).await;
    server.await.unwrap();
    result
}

#[tokio::test]
async fn swapping_clients_keeps_the_same_path_status_and_state() {
    let openai = run_openai(OPENAI_VALID).await.unwrap();
    let anthropic = run_anthropic(ANTHROPIC_VALID).await.unwrap();

    let path: Vec<&str> = openai.path.iter().map(|id| id.0.as_str()).collect();
    assert_eq!(path, ["draft", "record"]);
    assert_eq!(openai.status, RunStatus::Completed);
    assert_eq!(openai.state["recorded_priority"], "high");

    assert_eq!(anthropic.path, openai.path);
    assert_eq!(anthropic.status, openai.status);
    assert_eq!(anthropic.state, openai.state);
}

#[tokio::test]
async fn swapping_clients_keeps_the_same_schema_failure() {
    let openai = run_openai(OPENAI_INVALID).await.unwrap_err();
    let anthropic = run_anthropic(ANTHROPIC_INVALID).await.unwrap_err();
    assert!(
        matches!(openai, HarnessError::SchemaInvalid(_)),
        "{openai:?}"
    );
    assert_eq!(
        std::mem::discriminant(&anthropic),
        std::mem::discriminant(&openai)
    );
}
