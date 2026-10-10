#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, Instant};

use harness_core::{
    Capability, Effect, HarnessError, Machine, Next, Node, NodeId, Outcome, State, Workflow,
};
use harness_effects::Shell;
use serde_json::{Value, json};
use tempfile::TempDir;

fn effect_error(result: Result<Value, HarnessError>) -> String {
    match result {
        Err(HarnessError::Effect(msg)) => msg,
        other => panic!("expected an effect error, got {other:?}"),
    }
}

#[tokio::test]
async fn runs_the_fixed_command_and_ignores_input() {
    let shell = Shell::new("/bin/echo", ["hello"]).unwrap();
    let expected = json!({"status": 0, "stdout": "hello\n", "stderr": ""});
    assert_eq!(shell.run(json!({})).await.unwrap(), expected);
    assert_eq!(
        shell
            .run(json!({"command": "/bin/echo pwned", "args": ["pwned"]}))
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn non_zero_exit_is_data_not_an_error() {
    let out = Shell::new("/usr/bin/false", Vec::<&str>::new())
        .unwrap()
        .run(json!({}))
        .await
        .unwrap();
    assert_ne!(out["status"], 0);

    let out = Shell::new("/bin/ls", ["/definitely/not/here"])
        .unwrap()
        .run(json!({}))
        .await
        .unwrap();
    assert_ne!(out["status"], 0);
    assert!(!out["stderr"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn environment_is_cleared_except_explicit_vars() {
    let out = Shell::new("/usr/bin/printenv", ["FOO"])
        .unwrap()
        .env("FOO", "bar")
        .run(json!({}))
        .await
        .unwrap();
    assert_eq!(out["stdout"], "bar\n");

    assert!(std::env::var_os("HOME").is_some(), "test needs HOME set");
    let out = Shell::new("/usr/bin/printenv", ["HOME"])
        .unwrap()
        .run(json!({}))
        .await
        .unwrap();
    assert_ne!(out["status"], 0);
    assert_eq!(out["stdout"], "");
}

#[tokio::test]
async fn cwd_sets_the_working_directory() {
    let dir = TempDir::new().unwrap();
    let out = Shell::new("/bin/pwd", ["-P"])
        .unwrap()
        .cwd(dir.path())
        .unwrap()
        .run(json!({}))
        .await
        .unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(out["stdout"], format!("{}\n", canonical.display()));

    assert!(
        Shell::new("/bin/pwd", Vec::<&str>::new())
            .unwrap()
            .cwd(dir.path().join("missing"))
            .is_err()
    );
}

#[test]
fn construction_name_and_capability() {
    assert!(matches!(
        Shell::new("echo", ["hi"]),
        Err(HarnessError::Effect(_))
    ));
    assert!(Shell::new("", Vec::<&str>::new()).is_err());
    assert!(Shell::new("bin/echo", Vec::<&str>::new()).is_err());
    let shell = Shell::new("/bin/echo", Vec::<&str>::new()).unwrap();
    assert_eq!(shell.name(), "shell");
    assert_eq!(shell.capability(), Capability::DangerousShell);
    assert_eq!(shell.named("build").name(), "build");
}

#[tokio::test]
async fn non_utf8_oversized_and_slow_commands_are_errors() {
    let msg = effect_error(
        Shell::new("/usr/bin/printf", ["\\377"])
            .unwrap()
            .run(json!({}))
            .await,
    );
    assert!(msg.contains("UTF-8"), "{msg}");

    let started = Instant::now();
    let msg = effect_error(
        Shell::new("/usr/bin/yes", Vec::<&str>::new())
            .unwrap()
            .max_bytes(16)
            .run(json!({}))
            .await,
    );
    assert!(msg.contains("stdout is over the 16 byte limit"), "{msg}");
    assert!(started.elapsed() < Duration::from_secs(5));

    let started = Instant::now();
    let msg = effect_error(
        Shell::new("/bin/sleep", ["30"])
            .unwrap()
            .timeout(Duration::from_millis(200))
            .run(json!({}))
            .await,
    );
    assert!(msg.contains("timed out"), "{msg}");
    assert!(started.elapsed() < Duration::from_secs(5));

    let msg = effect_error(
        Shell::new("/no/such/program", Vec::<&str>::new())
            .unwrap()
            .run(json!({}))
            .await,
    );
    assert!(msg.contains("spawn"), "{msg}");
}

fn transition(_node: &NodeId, _outcome: &Outcome, _state: &State) -> Next {
    Next::End
}

fn workflow() -> Workflow {
    let nodes = vec![Node::Effect {
        id: "run".into(),
        name: "shell".into(),
        reads: vec!["command".into()],
    }];
    Workflow::new("run".into(), nodes, transition).unwrap()
}

/// State a model could have written. It is read by the node but never run.
fn hostile_state() -> State {
    State::from([("command".into(), json!("/bin/echo pwned"))])
}

fn touch(sentinel: &Path) -> Shell {
    Shell::new("/usr/bin/touch", [sentinel]).unwrap()
}

#[tokio::test]
async fn default_machine_has_no_shell_effect() {
    let dir = TempDir::new().unwrap();
    let sentinel = dir.path().join("ran");
    let _unregistered = touch(&sentinel);
    let machine = Machine::builder().grant(Capability::DangerousShell).build();
    assert_eq!(
        machine.run(&workflow(), hostile_state()).await.unwrap_err(),
        HarnessError::Workflow("unknown effect `shell`".into())
    );
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn machine_refuses_shell_without_the_capability() {
    let dir = TempDir::new().unwrap();
    let sentinel = dir.path().join("ran");
    let machine = Machine::builder().effect(touch(&sentinel)).build();
    assert_eq!(
        machine.run(&workflow(), hostile_state()).await.unwrap_err(),
        HarnessError::CapabilityDenied(Capability::DangerousShell)
    );
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn machine_runs_the_fixed_command_with_the_capability() {
    let machine = Machine::builder()
        .effect(Shell::new("/bin/echo", ["fixed"]).unwrap())
        .grant(Capability::DangerousShell)
        .build();
    let run = machine.run(&workflow(), hostile_state()).await.unwrap();
    assert_eq!(run.state["run"]["status"], 0);
    assert_eq!(run.state["run"]["stdout"], "fixed\n");
    assert_eq!(run.state["command"], "/bin/echo pwned");

    let dir = TempDir::new().unwrap();
    let sentinel = dir.path().join("ran");
    let machine = Machine::builder()
        .effect(touch(&sentinel))
        .grant(Capability::DangerousShell)
        .build();
    machine.run(&workflow(), hostile_state()).await.unwrap();
    assert!(sentinel.exists());
}
