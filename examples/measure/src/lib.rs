//! Micro-benchmark of two harness workflows against a ReAct-style loop.
//!
//! The harness side calls the ticket router and the gated-generation example.
//! The baseline calls the same OpenAI-compatible client the gated draft uses,
//! one schema-bounded step at a time, until `finish` or the step cap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_core::{
    EventKind, GenOutput, GenRequest, GenerativeModel, HarnessError, JsonSchema, OutputSpec, Run,
    Usage, validate_output,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Steps a ReAct loop may take before the run is branch `unfinished`.
pub const REACT_STEP_CAP: u32 = 4;

/// Repeats used when the command line omits `--repeats`.
pub const DEFAULT_REPEATS: usize = 10;

/// Token cap for one ticket-routing ReAct step.
pub const TICKET_MAX_TOKENS: u32 = 256;

/// Token cap for one gated-generation ReAct step.
pub const GATED_MAX_TOKENS: u32 = 512;

const TICKET_FIXTURE: &str = "examples/ticket-router/fixtures/billing.json";
const GATED_FIXTURE: &str = "examples/gated-generate/fixtures/allow.json";

/// One finished comparison run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub branch: String,
    pub latency_ms: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub steps: u32,
}

/// Means and branch stability computed from a series of [`RunRecord`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub runs: usize,
    pub modal_branch: String,
    pub stability: f64,
    pub branch_counts: BTreeMap<String, usize>,
    pub mean_latency_ms: f64,
    pub mean_input_tokens: Option<f64>,
    pub mean_output_tokens: Option<f64>,
}

/// Harness runs and ReAct runs for one workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Side {
    pub runs: Vec<RunRecord>,
    pub summary: Summary,
}

/// Raw results for one workflow. The summary is computed from `runs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementFile {
    pub workflow: String,
    pub fixture: String,
    pub repeats: usize,
    pub react_step_cap: u32,
    pub harness: Side,
    pub react: Side,
}

/// Models and protocol values the measurement note records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteMeta {
    pub openai_model: String,
    pub typesafe_model: String,
    pub repeats: usize,
}

/// What one ReAct step asked the loop to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Continue,
    Finish { branch: String },
}

/// Modal branch, stability, and means. An empty series is a workflow error.
///
/// Stability is the share of runs whose branch equals the most common branch.
/// A tie uses the lexicographically first label. A missing token anywhere in
/// the series leaves that mean as `None`.
pub fn summarize(runs: &[RunRecord]) -> Result<Summary, HarnessError> {
    if runs.is_empty() {
        return Err(HarnessError::Workflow(
            "cannot summarize an empty series".into(),
        ));
    }
    let mut branch_counts = BTreeMap::new();
    for run in runs {
        *branch_counts.entry(run.branch.clone()).or_insert(0) += 1;
    }
    let (modal_branch, modal_count) = branch_counts
        .iter()
        .max_by(|(left_branch, left_count), (right_branch, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_branch.cmp(left_branch))
        })
        .map(|(branch, count)| (branch.clone(), *count))
        .expect("branch counts are non-empty");
    let runs_len = runs.len();
    Ok(Summary {
        runs: runs_len,
        modal_branch,
        stability: modal_count as f64 / runs_len as f64,
        branch_counts,
        mean_latency_ms: runs.iter().map(|run| run.latency_ms as f64).sum::<f64>()
            / runs_len as f64,
        mean_input_tokens: mean_tokens(runs.iter().map(|run| run.input_tokens), runs_len),
        mean_output_tokens: mean_tokens(runs.iter().map(|run| run.output_tokens), runs_len),
    })
}

/// Cell text the evaluation table must contain for this summary.
pub fn table_cells(summary: &Summary) -> Vec<String> {
    let modal = summary
        .branch_counts
        .get(&summary.modal_branch)
        .copied()
        .unwrap_or(0);
    vec![
        summary.modal_branch.clone(),
        format!("{modal}/{}", summary.runs),
        format!("{:.1}", summary.mean_latency_ms),
        match summary.mean_input_tokens {
            Some(tokens) => format!("{tokens:.1}"),
            None => "omitted".into(),
        },
        match summary.mean_output_tokens {
            Some(tokens) => format!("{tokens:.1}"),
            None => "omitted".into(),
        },
    ]
}

/// Phrases the paper evaluation must contain for this summary.
pub fn quoted_figures(label: &str, summary: &Summary) -> Vec<String> {
    let modal = summary
        .branch_counts
        .get(&summary.modal_branch)
        .copied()
        .unwrap_or(0);
    vec![
        format!("{label} modal branch {}", summary.modal_branch),
        format!("{label} stability {modal}/{}", summary.runs),
        format!("{label} mean latency {:.1} ms", summary.mean_latency_ms),
        match summary.mean_input_tokens {
            Some(tokens) => format!("{label} mean input tokens {tokens:.1}"),
            None => format!("{label} mean input tokens omitted"),
        },
        match summary.mean_output_tokens {
            Some(tokens) => format!("{label} mean output tokens {tokens:.1}"),
            None => format!("{label} mean output tokens omitted"),
        },
    ]
}

/// Builds a measurement file after checking that both sides have `repeats` runs.
pub fn measurement_file(
    workflow: impl Into<String>,
    fixture: impl Into<String>,
    repeats: usize,
    harness: Vec<RunRecord>,
    react: Vec<RunRecord>,
) -> Result<MeasurementFile, HarnessError> {
    if harness.len() != repeats || react.len() != repeats {
        return Err(HarnessError::Workflow(format!(
            "expected {repeats} runs on each side, got {} harness and {} react",
            harness.len(),
            react.len()
        )));
    }
    Ok(MeasurementFile {
        workflow: workflow.into(),
        fixture: fixture.into(),
        repeats,
        react_step_cap: REACT_STEP_CAP,
        harness: Side {
            summary: summarize(&harness)?,
            runs: harness,
        },
        react: Side {
            summary: summarize(&react)?,
            runs: react,
        },
    })
}

/// The note that names the models, the fixtures, and the rerun command.
pub fn measurement_note(meta: &NoteMeta) -> String {
    format!(
        "\
# Measurements

These files are the raw runs for the paper evaluation. The harness side calls the ticket router and the gated-generation example. The baseline is a ReAct-style loop on the same OpenAI-compatible model the gated draft uses. `inspect` returns the fixture text. `finish` ends the loop. A loop that reaches the step cap is branch `unfinished`.

- OpenAI model: `{openai}`
- TypeSafe model: `{typesafe}`
- Ticket fixture: `{ticket_fixture}`
- Gated fixture: `{gated_fixture}`
- Repeats: {repeats}
- ReAct step cap: {cap}
- Ticket ReAct max tokens: {ticket_max}
- Gated ReAct max tokens: {gated_max}

Rerun:

```sh
cargo run -p measure -- --repeats {repeats}
```

The command reads `TYPESAFE_API_KEY`, `OPENAI_API_KEY`, and `OPENAI_MODEL` from the environment. `TYPESAFE_MODEL` overrides the System One model.
",
        openai = meta.openai_model,
        typesafe = meta.typesafe_model,
        ticket_fixture = TICKET_FIXTURE,
        gated_fixture = GATED_FIXTURE,
        repeats = meta.repeats,
        cap = REACT_STEP_CAP,
        ticket_max = TICKET_MAX_TOKENS,
        gated_max = GATED_MAX_TOKENS,
    )
}

/// `paper/measurements` relative to this crate.
pub fn measurements_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../paper/measurements")
}

/// Writes `{workflow}.json` under `dir`.
pub fn write_measurement(dir: &Path, file: &MeasurementFile) -> Result<(), HarnessError> {
    std::fs::create_dir_all(dir)
        .map_err(|err| HarnessError::Workflow(format!("creating {}: {err}", dir.display())))?;
    let path = dir.join(format!("{}.json", file.workflow));
    let body = serde_json::to_string_pretty(file)
        .map_err(|err| HarnessError::Workflow(format!("encoding measurement: {err}")))?;
    std::fs::write(&path, format!("{body}\n"))
        .map_err(|err| HarnessError::Workflow(format!("writing {}: {err}", path.display())))?;
    Ok(())
}

/// Writes `README.md` under `dir`.
pub fn write_note(dir: &Path, meta: &NoteMeta) -> Result<(), HarnessError> {
    std::fs::create_dir_all(dir)
        .map_err(|err| HarnessError::Workflow(format!("creating {}: {err}", dir.display())))?;
    let path = dir.join("README.md");
    std::fs::write(&path, measurement_note(meta))
        .map_err(|err| HarnessError::Workflow(format!("writing {}: {err}", path.display())))?;
    Ok(())
}

/// Destination and summed decide/generate usage from a harness run.
pub fn record_from_run(run: &Run) -> Result<RunRecord, HarnessError> {
    let branch = run
        .state
        .get("destination")
        .and_then(Value::as_str)
        .ok_or_else(|| HarnessError::Workflow("run has no destination".into()))?
        .to_owned();
    let mut usages = Vec::new();
    for event in run.events.iter() {
        if matches!(event.kind, EventKind::Decide | EventKind::Generate) {
            usages.push(event.usage.clone());
        }
    }
    if usages.is_empty() {
        return Err(HarnessError::Workflow(
            "run recorded no decide or generate step".into(),
        ));
    }
    let (latency_ms, input_tokens, output_tokens) = sum_usage(&usages)?;
    Ok(RunRecord {
        branch,
        latency_ms,
        input_tokens,
        output_tokens,
        steps: u32::try_from(usages.len())
            .map_err(|_| HarnessError::Workflow("step count does not fit u32".into()))?,
    })
}

/// Ticket text the router stores on `ticket.text`.
pub fn ticket_observation(fixture: &ticket_router::Fixture) -> String {
    format!(
        "{}\n{}",
        fixture.ticket.subject.trim(),
        fixture.ticket.body.trim()
    )
}

/// Request text the gated example stores after normalize.
pub fn gated_observation(fixture: &gated_generate::Fixture) -> String {
    fixture.request.trim().to_owned()
}

pub fn ticket_fixture_path() -> &'static str {
    TICKET_FIXTURE
}

pub fn gated_fixture_path() -> &'static str {
    GATED_FIXTURE
}

/// Schema for one ticket-routing ReAct step.
pub fn ticket_schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["thought", "action", "label"],
        "properties": {
            "thought": {"type": "string"},
            "action": {"type": "string", "enum": ["inspect", "finish"]},
            "label": {
                "type": "string",
                "enum": ["", "billing", "technical", "account", "queue"]
            }
        }
    })))
}

/// Schema for one gated-generation ReAct step.
pub fn gated_schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["thought", "action", "reply", "outcome"],
        "properties": {
            "thought": {"type": "string"},
            "action": {"type": "string", "enum": ["inspect", "draft", "finish"]},
            "reply": {"type": "string"},
            "outcome": {"type": "string", "enum": ["", "drafted", "safe"]}
        }
    })))
}

pub const TICKET_TASK: &str = "\
Route this support ticket. Alternate a thought with one action. \
action \"inspect\" reads the ticket; the observation in the context is that text. \
action \"finish\" ends the loop. Set label to billing, technical, account, or queue. \
Use queue when the ticket does not belong to billing, technical, or account.";

pub const GATED_TASK: &str = "\
Decide whether to draft a reply to this support request. \
Alternate a thought with one action. \
action \"inspect\" reads the request; the observation in the context is that text. \
action \"draft\" records a reply string. \
action \"finish\" ends the loop. Set outcome to drafted when a reply should be sent, or safe when it should not.";

/// Maps a ticket step onto continue or a route branch.
pub fn interpret_ticket(value: &Value) -> Result<StepOutcome, HarnessError> {
    let action = field(value, "action")?;
    match action {
        "inspect" => Ok(StepOutcome::Continue),
        "finish" => {
            let label = field(value, "label")?;
            if matches!(label, "billing" | "technical" | "account" | "queue") {
                Ok(StepOutcome::Finish {
                    branch: label.to_owned(),
                })
            } else {
                Err(HarnessError::Workflow(format!(
                    "finish label {label:?} is not a route"
                )))
            }
        }
        other => Err(HarnessError::Workflow(format!(
            "unknown ticket action {other:?}"
        ))),
    }
}

/// Maps a gated step onto continue or a destination branch.
pub fn interpret_gated(value: &Value) -> Result<StepOutcome, HarnessError> {
    let action = field(value, "action")?;
    match action {
        "inspect" | "draft" => Ok(StepOutcome::Continue),
        "finish" => {
            let outcome = field(value, "outcome")?;
            if matches!(outcome, "drafted" | "safe") {
                Ok(StepOutcome::Finish {
                    branch: outcome.to_owned(),
                })
            } else {
                Err(HarnessError::Workflow(format!(
                    "finish outcome {outcome:?} is not drafted or safe"
                )))
            }
        }
        other => Err(HarnessError::Workflow(format!(
            "unknown gated action {other:?}"
        ))),
    }
}

/// Runs the ReAct loop. `inspect` and `draft` keep the same observation.
/// The fourth step without `finish` is branch `unfinished`.
pub async fn react_loop(
    model: &dyn GenerativeModel,
    task: &str,
    observation: &str,
    schema: OutputSpec,
    max_tokens: u32,
    interpret: fn(&Value) -> Result<StepOutcome, HarnessError>,
) -> Result<RunRecord, HarnessError> {
    let mut transcript = Vec::new();
    let mut usages = Vec::new();
    for _ in 0..REACT_STEP_CAP {
        let response = model
            .complete(GenRequest {
                task: task.to_owned(),
                context: json!({
                    "observation": observation,
                    "steps": transcript,
                }),
                output: schema.clone(),
                max_tokens,
            })
            .await?;
        validate_output(&schema, &response.output)?;
        usages.push(response.usage.clone());
        let GenOutput::Json(value) = response.output else {
            return Err(HarnessError::SchemaInvalid(
                "ReAct step returned free text".into(),
            ));
        };
        if let StepOutcome::Finish { branch } = interpret(&value)? {
            let (latency_ms, input_tokens, output_tokens) = sum_usage(&usages)?;
            return Ok(RunRecord {
                branch,
                latency_ms,
                input_tokens,
                output_tokens,
                steps: u32::try_from(usages.len())
                    .map_err(|_| HarnessError::Workflow("step count does not fit u32".into()))?,
            });
        }
        transcript.push(value);
    }
    let (latency_ms, input_tokens, output_tokens) = sum_usage(&usages)?;
    Ok(RunRecord {
        branch: "unfinished".into(),
        latency_ms,
        input_tokens,
        output_tokens,
        steps: u32::try_from(usages.len())
            .map_err(|_| HarnessError::Workflow("step count does not fit u32".into()))?,
    })
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, HarnessError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| HarnessError::Workflow(format!("ReAct step is missing string field {key}")))
}

fn mean_tokens(tokens: impl Iterator<Item = Option<u32>>, runs: usize) -> Option<f64> {
    let mut total = 0u32;
    for value in tokens {
        total = total.checked_add(value?)?;
    }
    Some(total as f64 / runs as f64)
}

fn sum_usage(usages: &[Usage]) -> Result<(u64, Option<u32>, Option<u32>), HarnessError> {
    let mut latency_ms = 0u64;
    let mut input_tokens = Some(0u32);
    let mut output_tokens = Some(0u32);
    for usage in usages {
        latency_ms = latency_ms
            .checked_add(usage.latency_ms)
            .ok_or_else(|| HarnessError::Workflow("latency sum does not fit u64".into()))?;
        input_tokens = match (input_tokens, usage.input_tokens) {
            (Some(total), Some(next)) => total.checked_add(next),
            _ => None,
        };
        output_tokens = match (output_tokens, usage.output_tokens) {
            (Some(total), Some(next)) => total.checked_add(next),
            _ => None,
        };
    }
    Ok((latency_ms, input_tokens, output_tokens))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use harness_core::{GenResponse, RunStatus, Usage};
    use serde_json::json;

    use super::*;

    fn record(branch: &str, latency: u64, input: Option<u32>, output: Option<u32>) -> RunRecord {
        RunRecord {
            branch: branch.into(),
            latency_ms: latency,
            input_tokens: input,
            output_tokens: output,
            steps: 1,
        }
    }

    #[test]
    fn tie_uses_the_lexicographically_first_branch() {
        let summary = summarize(&[
            record("billing", 10, Some(4), Some(2)),
            record("account", 30, Some(6), Some(2)),
        ])
        .unwrap();
        assert_eq!(summary.modal_branch, "account");
        assert_eq!(summary.stability, 0.5);
        assert_eq!(summary.mean_latency_ms, 20.0);
        assert_eq!(summary.mean_input_tokens, Some(5.0));
        assert_eq!(summary.mean_output_tokens, Some(2.0));
        assert!(
            quoted_figures("ticket harness", &summary)
                .contains(&"ticket harness stability 1/2".to_owned())
        );
    }

    #[test]
    fn a_missing_token_omits_that_mean() {
        let summary = summarize(&[
            record("billing", 10, Some(4), None),
            record("billing", 20, None, Some(2)),
        ])
        .unwrap();
        assert_eq!(summary.modal_branch, "billing");
        assert_eq!(summary.stability, 1.0);
        assert_eq!(summary.mean_input_tokens, None);
        assert_eq!(summary.mean_output_tokens, None);
        let figures = quoted_figures("ticket ReAct", &summary);
        assert!(figures.contains(&"ticket ReAct mean input tokens omitted".to_owned()));
        assert!(figures.contains(&"ticket ReAct mean output tokens omitted".to_owned()));
        assert!(figures.contains(&"ticket ReAct mean latency 15.0 ms".to_owned()));
    }

    #[test]
    fn empty_series_is_an_error() {
        assert!(summarize(&[]).is_err());
    }

    #[test]
    fn note_names_the_models_fixtures_and_rerun_command() {
        let note = measurement_note(&NoteMeta {
            openai_model: "gpt-4.1-mini-2025-04-14".into(),
            typesafe_model: "jev-latest".into(),
            repeats: 10,
        });
        assert!(note.contains("gpt-4.1-mini-2025-04-14"));
        assert!(note.contains("jev-latest"));
        assert!(note.contains(TICKET_FIXTURE));
        assert!(note.contains(GATED_FIXTURE));
        assert!(note.contains("cargo run -p measure -- --repeats 10"));
        assert!(!note.contains("sk-"));
    }

    #[test]
    fn finish_requires_a_known_label() {
        assert!(interpret_ticket(&json!({"action": "inspect", "label": ""})).is_ok());
        assert!(matches!(
            interpret_ticket(&json!({"action": "finish", "label": "billing"})).unwrap(),
            StepOutcome::Finish { branch } if branch == "billing"
        ));
        assert!(interpret_ticket(&json!({"action": "finish", "label": ""})).is_err());
        assert!(interpret_gated(&json!({"action": "draft", "outcome": ""})).is_ok());
        assert!(interpret_gated(&json!({"action": "finish", "outcome": ""})).is_err());
    }

    #[tokio::test]
    async fn offline_fixtures_record_destination_and_usage() {
        let ticket =
            ticket_router::load_fixture(include_str!("../../ticket-router/fixtures/billing.json"))
                .unwrap();
        let ticket_run = ticket_router::run(&ticket, false).await.unwrap();
        assert_eq!(ticket_run.status, RunStatus::Completed);
        let ticket_record = record_from_run(&ticket_run).unwrap();
        assert_eq!(ticket_record.branch, "billing");
        assert_eq!(ticket_record.latency_ms, 140);
        assert_eq!(ticket_record.input_tokens, Some(96));
        assert_eq!(ticket_record.output_tokens, Some(24));
        assert_eq!(ticket_record.steps, 1);
        assert_eq!(
            ticket_observation(&ticket),
            "Refund for a duplicate charge\nI was charged twice on invoice 1042. Please refund the second payment."
        );

        let gated =
            gated_generate::load_fixture(include_str!("../../gated-generate/fixtures/allow.json"))
                .unwrap();
        let gated_run = gated_generate::run(
            &gated,
            None,
            &gated_generate::Approval {
                path: std::path::PathBuf::new(),
                token_present: true,
            },
        )
        .await
        .unwrap();
        let gated_record = record_from_run(&gated_run).unwrap();
        assert_eq!(gated_record.branch, "drafted");
        assert_eq!(gated_record.latency_ms, 410);
        assert_eq!(gated_record.input_tokens, Some(228));
        assert_eq!(gated_record.output_tokens, Some(76));
        assert_eq!(gated_record.steps, 2);
        assert_eq!(
            gated_observation(&gated),
            "Please refund the duplicate charge on invoice 1042."
        );
    }

    struct Script(Mutex<VecDeque<GenResponse>>);

    impl GenerativeModel for Script {
        fn complete(
            &self,
            _req: GenRequest,
        ) -> harness_core::BoxFuture<'_, Result<GenResponse, HarnessError>> {
            let next = self.0.lock().expect("script").pop_front();
            Box::pin(async move {
                next.ok_or_else(|| HarnessError::Provider("script exhausted".into()))
            })
        }
    }

    fn scripted(action: &str, label: &str) -> GenResponse {
        GenResponse {
            output: GenOutput::Json(json!({
                "thought": "look",
                "action": action,
                "label": label,
            })),
            usage: Usage {
                latency_ms: 10,
                input_tokens: Some(3),
                output_tokens: Some(1),
                cost_usd: None,
            },
        }
    }

    #[tokio::test]
    async fn react_loop_sums_steps_until_finish() {
        let model = Script(Mutex::new(VecDeque::from([
            scripted("inspect", ""),
            scripted("finish", "billing"),
        ])));
        let record = react_loop(
            &model,
            TICKET_TASK,
            "ticket",
            ticket_schema(),
            TICKET_MAX_TOKENS,
            interpret_ticket,
        )
        .await
        .unwrap();
        assert_eq!(record.branch, "billing");
        assert_eq!(record.steps, 2);
        assert_eq!(record.latency_ms, 20);
        assert_eq!(record.input_tokens, Some(6));
        assert_eq!(record.output_tokens, Some(2));
    }

    #[tokio::test]
    async fn react_loop_marks_the_step_cap_unfinished() {
        let model = Script(Mutex::new(VecDeque::from([
            scripted("inspect", ""),
            scripted("inspect", ""),
            scripted("inspect", ""),
            scripted("inspect", ""),
            scripted("finish", "billing"),
        ])));
        let record = react_loop(
            &model,
            TICKET_TASK,
            "ticket",
            ticket_schema(),
            TICKET_MAX_TOKENS,
            interpret_ticket,
        )
        .await
        .unwrap();
        assert_eq!(record.branch, "unfinished");
        assert_eq!(record.steps, 4);
    }

    #[test]
    fn paper_quotes_the_committed_measurements() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let tex = std::fs::read_to_string(root.join("paper/main.tex")).unwrap();
        let cases = ["ticket-router.json", "gated-generate.json"];
        for name in cases {
            let path = root.join("paper/measurements").join(name);
            let parsed: MeasurementFile = serde_json::from_str(
                &std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path:?}: {err}")),
            )
            .unwrap_or_else(|err| panic!("{path:?}: {err}"));
            let harness = summarize(&parsed.harness.runs).unwrap();
            let react = summarize(&parsed.react.runs).unwrap();
            assert_eq!(harness.modal_branch, parsed.harness.summary.modal_branch);
            assert_eq!(harness.branch_counts, parsed.harness.summary.branch_counts);
            assert_eq!(react.modal_branch, parsed.react.summary.modal_branch);
            assert_eq!(react.branch_counts, parsed.react.summary.branch_counts);
            assert_eq!(table_cells(&harness), table_cells(&parsed.harness.summary));
            assert_eq!(table_cells(&react), table_cells(&parsed.react.summary));
            for cell in table_cells(&harness).into_iter().chain(table_cells(&react)) {
                assert!(tex.contains(&cell), "paper missing {cell}");
            }
        }
    }
}
