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
const DEFAULT_HOST: &str = "example.com";

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

    let hosts = destination_hosts(std::env::var("HTTP_DESTINATIONS").ok().as_deref());
    let (read, read_events) = read_note(workspace).await?;
    let escaped = escape_rejected(workspace).await?;
    let http = http_rejected(&hosts).await?;
    let narrowed = example_outside_list(&hosts).await?;
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
    let narrowed = narrowed.map(|line| format!("\n{line}")).unwrap_or_default();
    Ok(format!(
        "{read}\n{escaped}\n{http}{narrowed}\n{denied}\n{granted}\nrun_id: {id}"
    ))
}

/// Hosts for this example's `HttpGet`. An unset, empty, or blank value keeps
/// the effect allowlist. `HTTP_DESTINATIONS` is not a credential.
fn destination_hosts(configured: Option<&str>) -> Vec<String> {
    let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return vec![DEFAULT_HOST.into()];
    };
    let hosts: Vec<String> = configured
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
        .collect();
    if hosts.is_empty() {
        vec![DEFAULT_HOST.into()]
    } else {
        hosts
    }
}

fn allows_default_host(hosts: &[String]) -> bool {
    hosts
        .iter()
        .any(|host| host.eq_ignore_ascii_case(DEFAULT_HOST))
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

async fn http_rejected(hosts: &[String]) -> Result<String, String> {
    let effect = HttpGet::new(hosts).map_err(show)?;
    let err = machine(effect, Some(Capability::HttpGet))
        .run(
            &workflow("get", "http_get", &["url"]).map_err(show)?,
            State::from([("url".into(), json!("https://not-allowed.example/"))]),
        )
        .await
        .expect_err("a host outside the allowlist must be rejected");
    expect_contains(&err, "not on the allowlist")
}

async fn example_outside_list(hosts: &[String]) -> Result<Option<String>, String> {
    if allows_default_host(hosts) {
        return Ok(None);
    }
    let effect = HttpGet::new(hosts).map_err(show)?;
    let err = machine(effect, Some(Capability::HttpGet))
        .run(
            &workflow("get", "http_get", &["url"]).map_err(show)?,
            State::from([("url".into(), json!("https://example.com/"))]),
        )
        .await
        .expect_err("a host outside the destination list must be rejected");
    expect_contains(&err, "not on the allowlist").map(Some)
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

#[cfg(test)]
mod tests {
    use super::{DEFAULT_HOST, destination_hosts};
    use harness_effects::HttpGet;
    use serde_json::json;

    #[test]
    fn unset_or_blank_keeps_the_effect_allowlist() {
        assert_eq!(destination_hosts(None), [DEFAULT_HOST]);
        assert_eq!(destination_hosts(Some("")), [DEFAULT_HOST]);
        assert_eq!(destination_hosts(Some("  ")), [DEFAULT_HOST]);
        assert_eq!(destination_hosts(Some(",")), [DEFAULT_HOST]);
    }

    #[test]
    fn narrowed_list_rejects_example_com_before_a_send() {
        let hosts = destination_hosts(Some("127.0.0.1"));
        assert_eq!(hosts, ["127.0.0.1"]);
        let effect = HttpGet::new(&hosts).unwrap();
        let err = effect
            .check(&json!({"url": "https://example.com/"}))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not on the allowlist"), "{msg}");
    }
}
