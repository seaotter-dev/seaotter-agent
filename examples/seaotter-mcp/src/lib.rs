//! Stdio MCP for a workflow document.
//!
//! The tools are `workflow_propose`, `workflow_show`, `workflow_save`,
//! `workflow_run`, and `grant_list`. `grant add` is a subcommand, not a tool.
//! A host permission argument is dropped before the tool runs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harness_core::{
    DecisionModel, HarnessError, Machine, RiskTier, Run, State, WorkflowDocument, apply_reply,
};
use serde_json::{Map, Value, json};

const TOOLS: [&str; 5] = [
    "workflow_propose",
    "workflow_show",
    "workflow_save",
    "workflow_run",
    "grant_list",
];

/// Grants and the workflow file for one directory.
pub struct Server {
    dir: PathBuf,
    proposed: Option<WorkflowDocument>,
}

impl Server {
    pub fn open(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            proposed: None,
        }
    }

    pub fn grant_add(&self, name: &str) -> Result<(), HarnessError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(HarnessError::Workflow("grant name is empty".into()));
        }
        let mut grants = self.read_grants()?;
        grants.insert(name.to_owned());
        self.write_grants(&grants)
    }

    pub fn grant_list(&self) -> Result<Vec<String>, HarnessError> {
        Ok(self.read_grants()?.into_iter().collect())
    }

    pub fn propose(
        &mut self,
        owner: &str,
        tier: &str,
        reply: &str,
    ) -> Result<WorkflowDocument, HarnessError> {
        let tier = match tier {
            "read" => RiskTier::Read,
            "write" => RiskTier::Write,
            other => {
                return Err(HarnessError::Workflow(format!("unknown tier `{other}`")));
            }
        };
        let doc = apply_reply(owner, tier, self.read_grants()?, reply)?;
        self.proposed = Some(doc.clone());
        Ok(doc)
    }

    pub fn save(&self) -> Result<(), HarnessError> {
        let Some(doc) = &self.proposed else {
            return Err(HarnessError::Workflow("no workflow to save".into()));
        };
        let text = serde_json::to_string(doc)
            .map_err(|err| HarnessError::Workflow(format!("workflow file: {err}")))?;
        std::fs::write(self.workflow_path(), format!("{text}\n")).map_err(|err| {
            HarnessError::Workflow(format!("writing {}: {err}", self.workflow_path().display()))
        })
    }

    pub fn show(&self) -> Result<String, HarnessError> {
        std::fs::read_to_string(self.workflow_path()).map_err(|err| {
            HarnessError::Workflow(format!("reading {}: {err}", self.workflow_path().display()))
        })
    }

    pub async fn run(&self) -> Result<Run, HarnessError> {
        let doc = self.load_workflow()?;
        Machine::builder()
            .build()
            .run_document(&doc, State::new())
            .await
    }

    pub async fn run_with(
        &self,
        decision: impl DecisionModel + 'static,
    ) -> Result<Run, HarnessError> {
        let doc = self.load_workflow()?;
        Machine::builder()
            .decision(decision)
            .build()
            .run_document(&doc, State::new())
            .await
    }

    /// One JSON-RPC message. A notification returns `None`.
    pub async fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if id.is_none() {
            return None;
        }
        let id = id.unwrap();
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": message
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "seaotter", "version": "0.1.0" }
            })),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                self.call_tool(message.get("params").unwrap_or(&Value::Null))
                    .await
            }
            other => Err(format!("unknown method `{other}`")),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": message }
            }),
        })
    }

    async fn call_tool(&mut self, params: &Value) -> Result<Value, String> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = arguments(params);
        let outcome = match name {
            "workflow_propose" => self
                .propose(
                    args.get("owner").and_then(Value::as_str).unwrap_or(""),
                    args.get("tier").and_then(Value::as_str).unwrap_or(""),
                    args.get("reply").and_then(Value::as_str).unwrap_or(""),
                )
                .map(|doc| serde_json::to_string(&doc).unwrap_or_default()),
            "workflow_show" => self.show(),
            "workflow_save" => self.save().map(|()| "saved".to_owned()),
            "workflow_run" => self.run().await.map(|run| {
                let path: Vec<_> = run.path.iter().map(|id| id.0.as_str()).collect();
                serde_json::to_string(&path).unwrap_or_default()
            }),
            "grant_list" => self.grant_list().and_then(|names| {
                serde_json::to_string(&names)
                    .map_err(|err| HarnessError::Workflow(format!("grant list: {err}")))
            }),
            other => Err(HarnessError::Workflow(format!("unknown tool `{other}`"))),
        };
        Ok(match outcome {
            Ok(text) => tool_text(text, false),
            Err(err) => tool_text(err.to_string(), true),
        })
    }

    fn read_grants(&self) -> Result<BTreeSet<String>, HarnessError> {
        let path = self.grants_path();
        if !path.exists() {
            return Ok(BTreeSet::new());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|err| HarnessError::Workflow(format!("reading {}: {err}", path.display())))?;
        let names: Vec<String> = serde_json::from_str(&text)
            .map_err(|err| HarnessError::Workflow(format!("grants file: {err}")))?;
        Ok(names.into_iter().collect())
    }

    fn write_grants(&self, grants: &BTreeSet<String>) -> Result<(), HarnessError> {
        let names: Vec<&str> = grants.iter().map(String::as_str).collect();
        let text = serde_json::to_string(&names)
            .map_err(|err| HarnessError::Workflow(format!("grants file: {err}")))?;
        let path = self.grants_path();
        std::fs::write(&path, format!("{text}\n"))
            .map_err(|err| HarnessError::Workflow(format!("writing {}: {err}", path.display())))
    }

    fn load_workflow(&self) -> Result<WorkflowDocument, HarnessError> {
        let text = self.show()?;
        serde_json::from_str(&text)
            .map_err(|err| HarnessError::Workflow(format!("workflow file: {err}")))
    }

    fn grants_path(&self) -> PathBuf {
        self.dir.join("grants.json")
    }

    fn workflow_path(&self) -> PathBuf {
        self.dir.join("workflow.json")
    }
}

fn arguments(params: &Value) -> Map<String, Value> {
    let mut args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    args.remove("host_permission");
    args.remove("permission");
    args
}

fn tool_definitions() -> Vec<Value> {
    TOOLS
        .into_iter()
        .map(|name| {
            let (description, properties, required) = match name {
                "workflow_propose" => (
                    "Apply a recorded reply with the grants already on disk.",
                    json!({
                        "owner": { "type": "string" },
                        "tier": { "type": "string" },
                        "reply": { "type": "string" }
                    }),
                    json!(["owner", "tier", "reply"]),
                ),
                "workflow_show" => ("Return the saved workflow document.", json!({}), json!([])),
                "workflow_save" => (
                    "Write the proposed workflow document to disk.",
                    json!({}),
                    json!([]),
                ),
                "workflow_run" => ("Run the saved workflow document.", json!({}), json!([])),
                "grant_list" => ("List the grant names on disk.", json!({}), json!([])),
                _ => unreachable!(),
            };
            json!({
                "name": name,
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": properties,
                    "required": required
                }
            })
        })
        .collect()
}

fn tool_text(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

pub fn workflow_file(dir: &Path) -> PathBuf {
    dir.join("workflow.json")
}
