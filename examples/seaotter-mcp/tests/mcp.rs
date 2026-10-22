use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_core::{
    Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, HarnessError, Usage,
};
use seaotter_mcp::{Server, workflow_file};
use serde_json::json;

const REPLY: &str = include_str!("../../../crates/harness-core/fixtures/consultant-reply.json");
const GATED: &str = include_str!("../../../crates/harness-core/fixtures/gated-draft.json");
const REJECTED: &str = r#"[
  {
    "op": "add_decide",
    "id": "other",
    "reads": ["ticket"],
    "questions": {
      "route": {
        "kind": "choice",
        "instructions": "Which team should handle this support ticket?",
        "criteria": {
          "billing": "Charges, refunds, invoices, and payment methods."
        }
      }
    },
    "policy": { "min_confidence": 0.7, "on_low": "safe_default" }
  },
  { "op": "add_grant", "name": "file_read" }
]"#;

struct LabelDecision {
    top: &'static str,
    confidence: f64,
}

impl DecisionModel for LabelDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let top = self.top.to_owned();
        let confidence = self.confidence;
        Box::pin(async move {
            let answers = req
                .questions
                .into_keys()
                .map(|key| {
                    (
                        key,
                        Answer {
                            probabilities: BTreeMap::from([(top.clone(), confidence)]),
                            confidence,
                        },
                    )
                })
                .collect();
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

fn dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("seaotter-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[tokio::test]
async fn tools_list_returns_the_five_workflow_tools() {
    let mut server = Server::open(dir("list"));
    let response = server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }))
        .await
        .unwrap();
    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "workflow_propose",
            "workflow_show",
            "workflow_save",
            "workflow_run",
            "grant_list"
        ]
    );
    assert!(!names.contains(&"grant_add"));
}

#[test]
fn grant_add_then_grant_list_returns_the_name() {
    let directory = dir("grants");
    let server = Server::open(&directory);
    server.grant_add("file_read").unwrap();
    assert_eq!(server.grant_list().unwrap(), vec!["file_read".to_owned()]);
}

#[tokio::test]
async fn propose_ignores_a_host_permission_and_the_file_stores_no_key() {
    let directory = dir("propose");
    let mut server = Server::open(&directory);
    let response = server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "workflow_propose",
                "arguments": {
                    "owner": "fixture",
                    "tier": "read",
                    "reply": REPLY,
                    "host_permission": "host-allow-blob"
                }
            }
        }))
        .await
        .unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert!(text.contains("Display this flowchart."), "{text}");
    assert!(text.contains("classify decide"), "{text}");
    assert!(text.contains("route equals billing"), "{text}");
    assert!(text.contains("safe_default"), "{text}");
    assert!(!text.contains("host_permission"), "{text}");
    assert!(!text.contains("host-allow-blob"), "{text}");

    server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "workflow_save", "arguments": {} }
        }))
        .await
        .unwrap();
    let saved = std::fs::read_to_string(workflow_file(&directory)).unwrap();
    assert!(saved.contains("\"owner\":\"fixture\""), "{saved}");
    assert!(saved.contains("\"tier\":\"read\""), "{saved}");
    assert!(saved.contains("classify"), "{saved}");
    for key in [
        "host_permission",
        "host-allow-blob",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "TYPESAFE_API_KEY",
        "COMPOSIO_API_KEY",
    ] {
        assert!(!saved.contains(key), "{key} in {saved}");
    }
}

#[tokio::test]
async fn show_returns_the_allow_condition() {
    let directory = dir("show");
    std::fs::write(workflow_file(&directory), GATED).unwrap();
    let mut server = Server::open(&directory);
    let response = server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "workflow_show", "arguments": {} }
        }))
        .await
        .unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(response["result"]["isError"], false);
    assert!(text.contains("Display this flowchart."), "{text}");
    assert!(text.contains("draft generate allow true"), "{text}");
}

#[tokio::test]
async fn a_rejected_propose_keeps_the_previous_graph() {
    let directory = dir("reject");
    let mut server = Server::open(&directory);
    server.propose("fixture", "read", REPLY).unwrap();
    let response = server
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "workflow_propose",
                "arguments": {
                    "owner": "fixture",
                    "tier": "read",
                    "reply": REJECTED
                }
            }
        }))
        .await
        .unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(text.contains("grant add is not a catalog call"), "{text}");
    assert!(!text.contains("Display this flowchart."), "{text}");
    server.save().unwrap();
    let saved = std::fs::read_to_string(workflow_file(&directory)).unwrap();
    assert!(saved.contains("classify"), "{saved}");
    assert!(!saved.contains("other"), "{saved}");
}

#[tokio::test]
async fn a_scripted_run_reaches_billing() {
    let directory = dir("run");
    let mut server = Server::open(&directory);
    server.propose("fixture", "read", REPLY).unwrap();
    server.save().unwrap();
    let run = server
        .run_with(LabelDecision {
            top: "billing",
            confidence: 0.88,
        })
        .await
        .unwrap();
    assert_eq!(run.path.last().map(|id| id.0.as_str()), Some("billing"));
}
