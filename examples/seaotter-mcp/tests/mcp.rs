use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_core::{
    Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, HarnessError, Usage,
};
use seaotter_mcp::{Server, workflow_file};
use serde_json::json;

const REPLY: &str = include_str!("../../../crates/harness-core/fixtures/consultant-reply.json");

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
    assert!(text.contains("classify"), "{text}");
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
