use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use harness_core::{HarnessError, append_run, read_run};
use ticket_router::{load_fixture, replay, report, run};

#[tokio::main]
async fn main() -> ExitCode {
    match execute().await {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

enum Mode {
    Run { live: bool },
    Replay(String),
}

async fn execute() -> Result<String, HarnessError> {
    let (mode, path) = parse_args()?;
    let json = std::fs::read_to_string(&path)
        .map_err(|err| HarnessError::Workflow(format!("reading {}: {err}", path.display())))?;
    let fixture = load_fixture(&json)?;
    match mode {
        Mode::Run { live } => {
            let finished = run(&fixture, live).await?;
            let user_id = env::var("USER_ID").unwrap_or_default();
            let id = append_run(
                std::path::Path::new("/data/log"),
                "examples/ticket-router",
                &user_id,
                finished.events.as_slice(),
            )?;
            Ok(format!("{}\nrun_id: {id}", report(&finished)))
        }
        Mode::Replay(id) => {
            let events = read_run(std::path::Path::new("/data/log"), &id)?;
            let finished = replay(&fixture, &events).await?;
            Ok(report(&finished))
        }
    }
}

fn parse_args() -> Result<(Mode, PathBuf), HarnessError> {
    let mut live = false;
    let mut replay_id = None;
    let mut path = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--live" {
            live = true;
        } else if arg == "--replay" {
            let id = args
                .next()
                .ok_or_else(|| HarnessError::Workflow("`--replay` needs a run id".into()))?;
            replay_id = Some(id);
        } else if path.is_some() {
            return Err(HarnessError::Workflow(format!(
                "unexpected argument `{arg}`"
            )));
        } else {
            path = Some(arg);
        }
    }
    if live && replay_id.is_some() {
        return Err(HarnessError::Workflow(
            "`--replay` does not take `--live`".into(),
        ));
    }
    let path = match path {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
            }
        }
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/billing.json"),
    };
    let mode = match replay_id {
        Some(id) => Mode::Replay(id),
        None => Mode::Run { live },
    };
    Ok((mode, path))
}
