use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use gated_generate::{Approval, LiveClient, load_fixture, report, run, token_is_present};
use harness_core::{HarnessError, append_run};

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

async fn execute() -> Result<String, HarnessError> {
    let (live, path) = parse_args()?;
    let json = std::fs::read_to_string(&path)
        .map_err(|err| HarnessError::Workflow(format!("reading {}: {err}", path.display())))?;
    let fixture = load_fixture(&json)?;
    let approval = Approval {
        path: PathBuf::from("/data/approval"),
        token_present: token_is_present(env::var("APPROVAL_TOKEN").ok().as_deref()),
    };
    let finished = run(&fixture, live, &approval).await?;
    let user_id = env::var("USER_ID").unwrap_or_default();
    let id = append_run(
        std::path::Path::new("/data/log"),
        "examples/gated-generate",
        &user_id,
        finished.events.as_slice(),
    )?;
    Ok(format!("{}\nrun_id: {id}", report(&finished)))
}

fn parse_args() -> Result<(Option<LiveClient>, PathBuf), HarnessError> {
    let mut live = None;
    let mut path = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--live" {
            let name = args.next().ok_or_else(|| {
                HarnessError::Workflow("`--live` needs `openai` or `anthropic`".into())
            })?;
            live = Some(match name.as_str() {
                "openai" => LiveClient::OpenAi,
                "anthropic" => LiveClient::Anthropic,
                other => {
                    return Err(HarnessError::Workflow(format!(
                        "`--live {other}` is not `openai` or `anthropic`"
                    )));
                }
            });
        } else if path.is_some() {
            return Err(HarnessError::Workflow(format!(
                "unexpected argument `{arg}`"
            )));
        } else {
            path = Some(arg);
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = match path {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        }
        None => root.join("fixtures/allow.json"),
    };
    Ok((live, path))
}
