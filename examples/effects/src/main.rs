//! Exercises FileRead, HttpGet, and the shell inside the container.
//!
//! There is no decide node and no model call. Effect names and the shell
//! argv are fixed in this file.

use std::path::Path;
use std::process::ExitCode;

use harness_core::{
    Capability, Effect, Event, HarnessError, Machine, Next, Node, NodeId, Outcome, State, Workflow,
    append_run,
};
use harness_effects::{FileRead, HttpGet, Shell};
use serde_json::json;

const WORKSPACE: &str = "/data/workspace";
const NOTE: &str = "workspace note";

#[tokio::main]
async fn main() -> ExitCode {
    match exercise().await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

async fn exercise() -> Result<String, String> {
    let workspace = Path::new(WORKSPACE);
    std::fs::create_dir_all(workspace)
        .map_err(|err| format!("creating {}: {err}", workspace.display()))?;
    let note = workspace.join("note.txt");
    std::fs::write(&note, NOTE).map_err(|err| format!("writing {}: {err}", note.display()))?;

    let (read, read_events) = read_note(workspace).await?;
    let escaped = escape_rejected(workspace).await?;
    let http = http_rejected().await?;
    let denied = shell_denied(workspace).await?;
    let (granted, shell_events) = shell_granted().await?;
    let mut events = read_events;
    events.extend(shell_events);
    let user_id = std::env::var("USER_ID").unwrap_or_default();
    let id = append_run(
        Path::new("/data/log"),
        "examples/effects",
        &user_id,
        &events,
    )
    .map_err(|err| err.to_string())?;
    Ok(format!(
        "{read}\n{escaped}\n{http}\n{denied}\n{granted}\nrun_id: {id}"
    ))
}

async fn read_note(root: &Path) -> Result<(String, Vec<Event>), String> {
    let run = machine(
        FileRead::new(root).map_err(show)?,
        Some(Capability::FileRead),
    )
    .run(
        &workflow("read", "file_read", &["path"]).map_err(show)?,
        State::from([("path".into(), json!("note.txt"))]),
    )
    .await
    .map_err(show)?;
    let contents = run.state["read"]["contents"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if contents != NOTE {
        return Err(format!("file_read returned {contents:?}"));
    }
    Ok((
        format!("file_read: {contents}"),
        run.events.as_slice().to_vec(),
    ))
}

async fn escape_rejected(root: &Path) -> Result<String, String> {
    let err = machine(
        FileRead::new(root).map_err(show)?,
        Some(Capability::FileRead),
    )
    .run(
        &workflow("read", "file_read", &["path"]).map_err(show)?,
        State::from([("path".into(), json!("../log"))]),
    )
    .await
    .expect_err("a path outside the workspace must be rejected");
    expect_contains(&err, "escapes the root")
}

async fn http_rejected() -> Result<String, String> {
    let effect = HttpGet::new(["example.com"]).map_err(show)?;
    let err = machine(effect, Some(Capability::HttpGet))
        .run(
            &workflow("get", "http_get", &["url"]).map_err(show)?,
            State::from([("url".into(), json!("https://not-allowed.example/"))]),
        )
        .await
        .expect_err("a host outside the allowlist must be rejected");
    expect_contains(&err, "not on the allowlist")
}

async fn shell_denied(root: &Path) -> Result<String, String> {
    let sentinel = root.join("shell-ran");
    let err = machine(
        Shell::new("/bin/touch", [sentinel.as_os_str()]).map_err(show)?,
        None,
    )
    .run(
        &workflow("run", "shell", &["command"]).map_err(show)?,
        State::from([("command".into(), json!("/bin/echo pwned"))]),
    )
    .await
    .expect_err("shell must stay off without a Rust grant");
    if sentinel.exists() {
        return Err(format!(
            "{} was created without a grant",
            sentinel.display()
        ));
    }
    expect_contains(&err, "capability denied: DangerousShell")
}

async fn shell_granted() -> Result<(String, Vec<Event>), String> {
    let run = machine(
        Shell::new("/bin/echo", ["fixed"]).map_err(show)?,
        Some(Capability::DangerousShell),
    )
    .run(
        &workflow("run", "shell", &["command"]).map_err(show)?,
        State::from([("command".into(), json!("/bin/echo pwned"))]),
    )
    .await
    .map_err(show)?;
    let stdout = run.state["run"]["stdout"].as_str().unwrap_or("");
    if stdout != "fixed\n" {
        return Err(format!("shell stdout was {stdout:?}"));
    }
    Ok(("shell: fixed".into(), run.events.as_slice().to_vec()))
}

fn machine(effect: impl Effect + 'static, grant: Option<Capability>) -> Machine {
    let mut builder = Machine::builder().effect(effect);
    if let Some(capability) = grant {
        builder = builder.grant(capability);
    }
    builder.build()
}

fn workflow(id: &str, effect: &str, reads: &[&str]) -> Result<Workflow, HarnessError> {
    Workflow::new(
        id.into(),
        vec![Node::Effect {
            id: id.into(),
            name: effect.into(),
            reads: reads.iter().map(|key| (*key).to_owned()).collect(),
        }],
        end,
    )
}

fn end(_node: &NodeId, _outcome: &Outcome, _state: &State) -> Next {
    Next::End
}

fn expect_contains(err: &HarnessError, needle: &str) -> Result<String, String> {
    let msg = show(err.clone());
    if msg.contains(needle) {
        Ok(msg)
    } else {
        Err(msg)
    }
}

fn show(err: HarnessError) -> String {
    err.to_string()
}
