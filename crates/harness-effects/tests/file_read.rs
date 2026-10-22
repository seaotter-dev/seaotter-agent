use std::fs;
use std::path::PathBuf;

use harness_core::{
    Capability, Effect, HarnessError, Machine, Next, Node, NodeId, Outcome, State, Workflow,
};
use harness_effects::FileRead;
use serde_json::{Value, json};
use tempfile::TempDir;

/// `<tmp>/root/{hello.txt, notes/a.txt}` and `<tmp>/outside/secret.txt`.
struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    secret: PathBuf,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("hello.txt"), "hello, world").unwrap();
    fs::write(root.join("notes/a.txt"), "note a").unwrap();
    let secret = outside.join("secret.txt");
    fs::write(&secret, "top secret").unwrap();
    Fixture {
        _dir: dir,
        root,
        secret,
    }
}

async fn read(effect: &FileRead, path: impl Into<Value>) -> Result<Value, HarnessError> {
    effect.run(json!({"path": path.into()})).await
}

fn effect_error(result: Result<Value, HarnessError>) -> String {
    match result {
        Err(HarnessError::Effect(msg)) => msg,
        other => panic!("expected an effect error, got {other:?}"),
    }
}

fn assert_escapes(result: Result<Value, HarnessError>) {
    let msg = effect_error(result);
    assert!(msg.contains("escapes the root"), "{msg}");
}

#[tokio::test]
async fn file_inside_the_prefix_is_returned_as_json() {
    let fx = fixture();
    let effect = FileRead::new(&fx.root).unwrap();
    assert_eq!(
        read(&effect, "hello.txt").await.unwrap(),
        json!({"path": "hello.txt", "contents": "hello, world", "bytes": 12})
    );
    assert_eq!(
        read(&effect, "./notes/a.txt").await.unwrap()["contents"],
        "note a"
    );
}

#[tokio::test]
async fn paths_that_escape_the_prefix_are_rejected() {
    let fx = fixture();
    let effect = FileRead::new(&fx.root).unwrap();
    assert_escapes(read(&effect, "../outside/secret.txt").await);
    assert_escapes(read(&effect, "notes/../../outside/secret.txt").await);
    assert_escapes(read(&effect, "notes/../hello.txt").await);
    assert_escapes(read(&effect, fx.secret.to_str().unwrap()).await);
    assert_escapes(read(&effect, "").await);
    assert_escapes(read(&effect, ".").await);
}

#[tokio::test]
async fn escaping_paths_are_rejected_before_touching_the_filesystem() {
    let fx = fixture();
    let effect = FileRead::new(&fx.root).unwrap();
    // The target does not exist: an "escapes" error (not "not found") shows
    // the path was rejected without looking it up.
    assert_escapes(read(&effect, "../outside/does-not-exist.txt").await);
    assert_escapes(read(&effect, "/definitely/not/a/real/path.txt").await);
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_that_resolve_outside_are_rejected() {
    let fx = fixture();
    std::os::unix::fs::symlink(&fx.secret, fx.root.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(fx.secret.parent().unwrap(), fx.root.join("out")).unwrap();
    std::os::unix::fs::symlink(fx.root.join("hello.txt"), fx.root.join("inner.txt")).unwrap();
    let effect = FileRead::new(&fx.root).unwrap();
    assert_escapes(read(&effect, "link.txt").await);
    assert_escapes(read(&effect, "out/secret.txt").await);
    assert_eq!(
        read(&effect, "inner.txt").await.unwrap()["contents"],
        "hello, world"
    );
}

#[tokio::test]
async fn bad_targets_and_inputs_are_effect_errors() {
    let fx = fixture();
    fs::write(fx.root.join("big.txt"), "x".repeat(32)).unwrap();
    fs::write(fx.root.join("binary.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let effect = FileRead::new(&fx.root).unwrap().max_bytes(16);

    assert!(effect_error(read(&effect, "missing.txt").await).contains("not found"));
    assert!(effect_error(read(&effect, "notes").await).contains("not a file"));
    assert!(effect_error(read(&effect, "big.txt").await).contains("limit"));
    assert!(effect_error(read(&effect, "binary.bin").await).contains("UTF-8"));
    assert!(effect_error(effect.run(json!({})).await).contains("`path`"));
    assert!(effect_error(read(&effect, 7).await).contains("`path`"));
}

#[test]
fn root_must_be_an_existing_directory() {
    let fx = fixture();
    assert!(matches!(
        FileRead::new(fx.root.join("nope")),
        Err(HarnessError::Effect(_))
    ));
    assert!(matches!(
        FileRead::new(fx.root.join("hello.txt")),
        Err(HarnessError::Effect(_))
    ));
    let effect = FileRead::new(&fx.root).unwrap();
    assert_eq!(effect.root(), fs::canonicalize(&fx.root).unwrap());
}

#[test]
fn name_and_capability() {
    let fx = fixture();
    let effect = FileRead::new(&fx.root).unwrap();
    assert_eq!(effect.name(), "file_read");
    assert_eq!(effect.capability(), Capability::FileRead);
    assert_eq!(effect.named("docs").name(), "docs");
}

fn choose_path(state: &mut State) -> Result<(), HarnessError> {
    state.insert("path".into(), json!("notes/a.txt"));
    Ok(())
}

fn transition(node: &NodeId, _outcome: &Outcome, _state: &State) -> Next {
    match node.0.as_str() {
        "choose" => Next::Goto("read".into()),
        _ => Next::End,
    }
}

fn workflow() -> Workflow {
    let nodes = vec![
        Node::Deterministic {
            id: "choose".into(),
            step: choose_path,
        },
        Node::Effect {
            id: "read".into(),
            name: "file_read".into(),
            reads: vec!["path".into()],
            connection: None,
            action: None,
        },
    ];
    Workflow::new("choose".into(), nodes, transition).unwrap()
}

#[tokio::test]
async fn machine_refuses_the_effect_without_the_capability() {
    let fx = fixture();
    let machine = Machine::builder()
        .effect(FileRead::new(&fx.root).unwrap())
        .build();
    assert_eq!(
        machine.run(&workflow(), State::new()).await.unwrap_err(),
        HarnessError::CapabilityDenied(Capability::FileRead)
    );
}

#[tokio::test]
async fn machine_runs_the_effect_with_the_capability() {
    let fx = fixture();
    let machine = Machine::builder()
        .effect(FileRead::new(&fx.root).unwrap())
        .grant(Capability::FileRead)
        .build();
    let run = machine.run(&workflow(), State::new()).await.unwrap();
    assert_eq!(run.state["read"]["contents"], "note a");
}
