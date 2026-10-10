//! Runs the live micro-benchmark and writes `paper/measurements/`.
//!
//! One provider error fails the process. Files are written only after every
//! repeat of both workflows has finished.

use std::env;
use std::process::ExitCode;

use gated_generate::LiveClient;
use harness_core::HarnessError;
use harness_providers::{OpenAiCompatible, OpenAiCompatibleConfig, TypeSafeConfig};
use measure::{
    DEFAULT_REPEATS, GATED_MAX_TOKENS, GATED_TASK, NoteMeta, TICKET_MAX_TOKENS, TICKET_TASK,
    gated_fixture_path, gated_observation, gated_schema, interpret_gated, interpret_ticket,
    measurement_file, measurements_dir, react_loop, record_from_run, ticket_fixture_path,
    ticket_observation, ticket_schema, write_measurement, write_note,
};

#[tokio::main]
async fn main() -> ExitCode {
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

async fn execute() -> Result<(), HarnessError> {
    let repeats = parse_repeats()?;
    let ticket = load_ticket()?;
    let gated = load_gated()?;
    let typesafe_model = TypeSafeConfig::from_env()?.model;
    let openai_config = OpenAiCompatibleConfig::from_env("OPENAI")?;
    let openai_model = openai_config.model.clone();
    let model = OpenAiCompatible::new(openai_config)?;

    let mut ticket_harness = Vec::with_capacity(repeats);
    let mut ticket_react = Vec::with_capacity(repeats);
    let ticket_text = ticket_observation(&ticket);
    for index in 1..=repeats {
        eprintln!("ticket-router harness {index}/{repeats}");
        ticket_harness.push(record_from_run(&ticket_router::run(&ticket, true).await?)?);
        eprintln!("ticket-router ReAct {index}/{repeats}");
        ticket_react.push(
            react_loop(
                &model,
                TICKET_TASK,
                &ticket_text,
                ticket_schema(),
                TICKET_MAX_TOKENS,
                interpret_ticket,
            )
            .await?,
        );
    }

    let mut gated_harness = Vec::with_capacity(repeats);
    let mut gated_react = Vec::with_capacity(repeats);
    let gated_text = gated_observation(&gated);
    for index in 1..=repeats {
        eprintln!("gated-generate harness {index}/{repeats}");
        gated_harness.push(record_from_run(
            &gated_generate::run(&gated, Some(LiveClient::OpenAi)).await?,
        )?);
        eprintln!("gated-generate ReAct {index}/{repeats}");
        gated_react.push(
            react_loop(
                &model,
                GATED_TASK,
                &gated_text,
                gated_schema(),
                GATED_MAX_TOKENS,
                interpret_gated,
            )
            .await?,
        );
    }

    let dir = measurements_dir();
    write_measurement(
        &dir,
        &measurement_file(
            "ticket-router",
            ticket_fixture_path(),
            repeats,
            ticket_harness,
            ticket_react,
        )?,
    )?;
    write_measurement(
        &dir,
        &measurement_file(
            "gated-generate",
            gated_fixture_path(),
            repeats,
            gated_harness,
            gated_react,
        )?,
    )?;
    write_note(
        &dir,
        &NoteMeta {
            openai_model,
            typesafe_model,
            repeats,
        },
    )?;
    println!("wrote {}", dir.display());
    Ok(())
}

fn parse_repeats() -> Result<usize, HarnessError> {
    let mut args = env::args().skip(1);
    let mut repeats = DEFAULT_REPEATS;
    while let Some(arg) = args.next() {
        if arg == "--repeats" {
            let value = args.next().ok_or_else(|| {
                HarnessError::Workflow("--repeats needs a positive integer".into())
            })?;
            repeats = value.parse().map_err(|_| {
                HarnessError::Workflow(format!("--repeats {value:?} is not a positive integer"))
            })?;
            if repeats == 0 {
                return Err(HarnessError::Workflow(
                    "--repeats needs a positive integer".into(),
                ));
            }
        } else {
            return Err(HarnessError::Workflow(format!("unknown argument {arg}")));
        }
    }
    Ok(repeats)
}

fn load_ticket() -> Result<ticket_router::Fixture, HarnessError> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ticket-router/fixtures/billing.json");
    let json = std::fs::read_to_string(&path)
        .map_err(|err| HarnessError::Workflow(format!("reading {}: {err}", path.display())))?;
    ticket_router::load_fixture(&json)
}

fn load_gated() -> Result<gated_generate::Fixture, HarnessError> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../gated-generate/fixtures/allow.json");
    let json = std::fs::read_to_string(&path)
        .map_err(|err| HarnessError::Workflow(format!("reading {}: {err}", path.display())))?;
    gated_generate::load_fixture(&json)
}
