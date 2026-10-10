use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use ticket_router::{load_fixture, report, run};

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

async fn execute() -> Result<String, harness_core::HarnessError> {
    let (live, path) = parse_args();
    let json = std::fs::read_to_string(&path).map_err(|err| {
        harness_core::HarnessError::Workflow(format!("reading {}: {err}", path.display()))
    })?;
    let fixture = load_fixture(&json)?;
    let finished = run(&fixture, live).await?;
    Ok(report(&finished))
}

fn parse_args() -> (bool, PathBuf) {
    let mut live = false;
    let mut path = None;
    for arg in env::args().skip(1) {
        if arg == "--live" {
            live = true;
        } else {
            path = Some(arg);
        }
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
    (live, path)
}
