use std::io::{IsTerminal, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::SystemTime;

use harness_core::{
    Action, Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, Effect, GenOutput,
    GenRequest, GenResponse, GenerativeModel, HarnessError, JsonSchema, OutputSpec, Usage, choose,
    validate_output,
};
use harness_effects::{FileRead, Search, Shell};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (_workflow, bin, rest) = match parse(&args) {
        Ok(Request::Health) => return ExitCode::SUCCESS,
        Ok(Request::Session) => {
            let dir = match std::env::current_dir() {
                Ok(dir) => dir,
                Err(err) => {
                    eprintln!("{err}");
                    return ExitCode::FAILURE;
                }
            };
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout().lock();
            let turn = OfflineTurn::new(dir.clone());
            let terminal = stdout.is_terminal();
            if let Err(err) = read_prompt(&dir, stdin.lock(), &mut stdout, &turn, &turn, terminal) {
                eprintln!("{err}");
                return ExitCode::FAILURE;
            }
            return ExitCode::SUCCESS;
        }
        Ok(Request::Continue) => {
            let dir = match std::env::current_dir() {
                Ok(dir) => dir,
                Err(err) => {
                    eprintln!("{err}");
                    return ExitCode::FAILURE;
                }
            };
            match continue_session(&dir) {
                Ok(text) => print!("{text}"),
                Err(err) => {
                    eprintln!("{err}");
                    return ExitCode::FAILURE;
                }
            }
            return ExitCode::SUCCESS;
        }
        Ok(Request::Print(message)) => {
            print!("{}", print_reply(message));
            return ExitCode::SUCCESS;
        }
        Ok(Request::Json(message)) => {
            print!("{}", json_events(message));
            return ExitCode::SUCCESS;
        }
        Ok(Request::Rpc) => {
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout().lock();
            if let Err(err) = serve_rpc_stream(stdin.lock(), &mut stdout) {
                eprintln!("{err}");
                return ExitCode::FAILURE;
            }
            return ExitCode::SUCCESS;
        }
        Ok(Request::Workflow(workflow, bin, rest)) => (workflow, bin, rest),
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let code = match Command::new(bin).args(rest).status() {
        Ok(status) => u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
        Err(err) => {
            eprintln!("running {bin}: {err}");
            return ExitCode::FAILURE;
        }
    };
    if code == 0
        && let Err(err) = ensure_workspace(Path::new("/data"))
    {
        eprintln!("{err}");
        return ExitCode::FAILURE;
    }
    ExitCode::from(code)
}

#[derive(Debug)]
enum Request<'a> {
    Health,
    Session,
    Continue,
    Print(&'a str),
    Json(&'a str),
    Rpc,
    Workflow(&'a str, &'static str, &'a [String]),
}

fn parse(args: &[String]) -> Result<Request<'_>, String> {
    if args.is_empty() {
        return Ok(Request::Session);
    }
    if args.len() == 1 && args[0] == "--health" {
        return Ok(Request::Health);
    }
    if args.len() == 1 && args[0] == "--continue" {
        return Ok(Request::Continue);
    }
    if let Some(request) = parse_output(args)? {
        return Ok(request);
    }
    let (workflow, bin, rest) = parse_workflow(args)?;
    Ok(Request::Workflow(workflow, bin, rest))
}

fn parse_output(args: &[String]) -> Result<Option<Request<'_>>, String> {
    match args.first().map(String::as_str) {
        Some("--print") => {
            if args.len() == 1 {
                return Err("--print needs a message".into());
            }
            if args.len() != 2 {
                return Err("--print takes one message".into());
            }
            Ok(Some(Request::Print(&args[1])))
        }
        Some("--mode") => {
            let mode = args.get(1).ok_or("--mode needs json")?;
            match mode.as_str() {
                "json" => {
                    if args.len() == 2 {
                        return Err("--mode json needs a message".into());
                    }
                    if args.len() != 3 {
                        return Err("--mode json takes one message".into());
                    }
                    Ok(Some(Request::Json(&args[2])))
                }
                "rpc" => {
                    if args.len() != 2 {
                        return Err("--mode rpc takes no message".into());
                    }
                    Ok(Some(Request::Rpc))
                }
                other => Err(format!("unknown mode `{other}`")),
            }
        }
        _ => Ok(None),
    }
}

fn print_reply(message: &str) -> String {
    format!("{message}\n")
}

fn json_events(message: &str) -> String {
    let decision = serde_json::json!({"kind": "decision", "action": "read"});
    let effect = serde_json::json!({"kind": "effect", "name": "read"});
    let reply = serde_json::json!({"kind": "reply", "text": message});
    format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&decision).expect("decision"),
        serde_json::to_string(&effect).expect("effect"),
        serde_json::to_string(&reply).expect("reply"),
    )
}

enum RpcStep {
    Continue(String),
    Stop(String),
}

#[cfg(test)]
fn serve_rpc(input: &str) -> String {
    let mut out = Vec::new();
    serve_rpc_stream(std::io::Cursor::new(input), &mut out).expect("rpc");
    String::from_utf8(out).expect("rpc")
}

fn serve_rpc_stream(
    input: impl std::io::BufRead,
    output: &mut impl std::io::Write,
) -> Result<(), String> {
    let mut remembered = None;
    for line in input.lines() {
        let line = line.map_err(|err| err.to_string())?;
        if line.is_empty() {
            continue;
        }
        let step = command_output(&line, &mut remembered);
        let stop = matches!(step, RpcStep::Stop(_));
        let text = match step {
            RpcStep::Continue(text) | RpcStep::Stop(text) => text,
        };
        output
            .write_all(text.as_bytes())
            .map_err(|err| err.to_string())?;
        output.flush().map_err(|err| err.to_string())?;
        if stop {
            break;
        }
    }
    Ok(())
}

fn command_output(line: &str, remembered: &mut Option<String>) -> RpcStep {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => return RpcStep::Stop(response_error(None, &err.to_string())),
    };
    let command = value.get("command").and_then(|command| command.as_str());
    match command {
        Some("prompt") => match value.get("message").and_then(|message| message.as_str()) {
            Some(message) => {
                remembered.replace(message.to_string());
                RpcStep::Continue(events_and_response(message, "prompt"))
            }
            None => RpcStep::Continue(response_error(Some("prompt"), "message")),
        },
        Some("continue") => match remembered.clone() {
            Some(message) => RpcStep::Continue(events_and_response(&message, "continue")),
            None => RpcStep::Continue(response_error(Some("continue"), "no prompt")),
        },
        Some("abort") => RpcStep::Stop(response_line("abort")),
        Some(other) => RpcStep::Stop(response_error(None, &format!("unknown command `{other}`"))),
        None => RpcStep::Stop(response_error(None, "missing command")),
    }
}

fn events_and_response(message: &str, command: &str) -> String {
    let mut text = json_events(message);
    text.push_str(&response_line(command));
    text
}

fn response_line(command: &str) -> String {
    let value = serde_json::json!({"kind": "response", "command": command});
    format!("{}\n", serde_json::to_string(&value).expect("response"))
}

fn response_error(command: Option<&str>, error: &str) -> String {
    let value = match command {
        Some(command) => {
            serde_json::json!({"kind": "response", "command": command, "error": error})
        }
        None => serde_json::json!({"kind": "response", "error": error}),
    };
    format!("{}\n", serde_json::to_string(&value).expect("response"))
}

fn system_prompt(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("AGENTS.md")).unwrap_or_default()
}

fn session(dir: &Path) -> String {
    let prompt = system_prompt(dir);
    if prompt.is_empty() {
        format!("{}\n> \n", dir.display())
    } else if prompt.ends_with('\n') {
        format!("{}\n{prompt}> \n", dir.display())
    } else {
        format!("{}\n{prompt}\n> \n", dir.display())
    }
}

const CONTEXT_LIMIT: usize = 1024;
const SUMMARY_MAX: usize = 256;
const RECENT_TURNS: usize = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    id: String,
    parent: Option<String>,
    text: String,
}

fn start_session(dir: &Path) -> Result<String, String> {
    let text = session(dir);
    let sessions = sessions_dir(dir);
    std::fs::create_dir_all(&sessions)
        .map_err(|err| format!("creating {}: {err}", sessions.display()))?;
    let path = sessions.join(format!("{}.jsonl", session_file_id()));
    let entry = Entry {
        id: "1".into(),
        parent: None,
        text: text.clone(),
    };
    let line = serde_json::to_string(&entry).map_err(|err| err.to_string())?;
    std::fs::write(&path, format!("{line}\n"))
        .map_err(|err| format!("writing {}: {err}", path.display()))?;
    Ok(text)
}

#[derive(Clone, Copy)]
enum Role {
    Person,
    Tool,
    Reply,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Person => "person",
            Role::Tool => "tool",
            Role::Reply => "reply",
        }
    }
}

struct TranscriptLine {
    role: Role,
    text: String,
}

struct Screen {
    rows: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

fn layout(header: &[String], lines: &[TranscriptLine], height: usize, prompt: &str) -> Screen {
    let mut content: Vec<String> = header.to_vec();
    for line in lines {
        content.push(format!("{} {}", line.role.name(), line.text));
    }
    let room = height.saturating_sub(1);
    if content.len() > room {
        content.drain(0..content.len() - room);
    }
    let mut rows = Vec::with_capacity(room + 1);
    rows.extend(std::iter::repeat_with(String::new).take(room.saturating_sub(content.len())));
    rows.extend(content);
    rows.push(prompt.to_string());
    Screen {
        cursor_row: rows.len() - 1,
        cursor_col: prompt.chars().count(),
        rows,
    }
}

fn header_lines(session_text: &str) -> Vec<String> {
    let body = session_text.strip_suffix("> \n").unwrap_or(session_text);
    body.lines().map(str::to_string).collect()
}

struct Frame<'a, W: Write> {
    output: &'a mut W,
    terminal: bool,
    header: Vec<String>,
    lines: Vec<TranscriptLine>,
    prompt: String,
    alternate: bool,
}

impl<'a, W: Write> Frame<'a, W> {
    fn new(output: &'a mut W, terminal: bool) -> Self {
        Self {
            output,
            terminal,
            header: Vec::new(),
            lines: Vec::new(),
            prompt: "> ".into(),
            alternate: false,
        }
    }

    fn emit_line(&mut self, role: Role, text: &str) -> Result<(), String> {
        let plain = if text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{text}\n")
        };
        if !self.terminal {
            return write_prompt(self.output, &plain);
        }
        self.lines.push(TranscriptLine {
            role,
            text: text.trim_end_matches(['\r', '\n']).to_string(),
        });
        self.paint()
    }

    fn emit_prompt(&mut self, prompt: &str, plain: &str) -> Result<(), String> {
        if !self.terminal {
            return write_prompt(self.output, plain);
        }
        self.prompt = prompt.to_string();
        self.paint()
    }

    fn emit_session(&mut self, text: &str) -> Result<(), String> {
        if !self.terminal {
            return write_prompt(self.output, text);
        }
        self.header = header_lines(text);
        self.prompt = "> ".into();
        self.paint()
    }

    fn paint(&mut self) -> Result<(), String> {
        let height = crossterm::terminal::size()
            .map(|(_, rows)| rows as usize)
            .unwrap_or(24)
            .max(1);
        let screen = layout(&self.header, &self.lines, height, &self.prompt);
        if !self.alternate {
            crossterm::execute!(self.output, crossterm::terminal::EnterAlternateScreen)
                .map_err(|err| err.to_string())?;
            self.alternate = true;
        }
        crossterm::execute!(
            self.output,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0),
        )
        .map_err(|err| err.to_string())?;
        for (index, row) in screen.rows.iter().enumerate() {
            if index + 1 == screen.rows.len() {
                write!(self.output, "{row}").map_err(|err| err.to_string())?;
            } else {
                writeln!(self.output, "{row}").map_err(|err| err.to_string())?;
            }
        }
        crossterm::execute!(
            self.output,
            crossterm::cursor::MoveTo(screen.cursor_col as u16, screen.cursor_row as u16),
        )
        .map_err(|err| err.to_string())?;
        self.output.flush().map_err(|err| err.to_string())
    }

    fn leave(&mut self) {
        if !self.alternate {
            return;
        }
        let _ = crossterm::execute!(self.output, crossterm::terminal::LeaveAlternateScreen);
        self.alternate = false;
    }
}

fn read_prompt(
    dir: &Path,
    mut input: impl std::io::BufRead,
    output: &mut impl Write,
    model: &dyn DecisionModel,
    writer: &dyn GenerativeModel,
    terminal: bool,
) -> Result<(), String> {
    let mut frame = Frame::new(output, terminal);
    let result = read_prompt_with(dir, &mut input, model, writer, &mut frame);
    frame.leave();
    result
}

fn read_prompt_with<W: Write>(
    dir: &Path,
    input: &mut impl std::io::BufRead,
    model: &dyn DecisionModel,
    writer: &dyn GenerativeModel,
    frame: &mut Frame<'_, W>,
) -> Result<(), String> {
    mark_interrupted(dir, frame)?;
    let text = start_session(dir)?;
    frame.emit_session(&text)?;
    loop {
        let Some(line) = read_one(input)? else {
            return Ok(());
        };
        if line.is_empty() {
            frame.emit_prompt("> ", "> \n")?;
            continue;
        }
        if line == "/quit" {
            return Ok(());
        }
        append_line(dir, &line)?;
        frame.emit_line(Role::Person, &line)?;
        if let Err(err) = run_turn(dir, &line, input, model, writer, frame) {
            eprintln!("{err}");
        }
        frame.emit_prompt("> ", "> \n")?;
    }
}

fn read_one(input: &mut impl std::io::BufRead) -> Result<Option<String>, String> {
    let mut buf = String::new();
    let n = input.read_line(&mut buf).map_err(|err| err.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }
    Ok(Some(buf))
}

fn mark_interrupted<W: Write>(dir: &Path, frame: &mut Frame<'_, W>) -> Result<(), String> {
    let path = match newest_session(dir) {
        Ok(path) => path,
        Err(err) if err == "no session" => return Ok(()),
        Err(err) => return Err(err),
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    let entries = parse_entries(&path, &raw)?;
    let Some(last) = entries.last() else {
        return Ok(());
    };
    if !last.text.starts_with("shell ") {
        return Ok(());
    }
    append_line(dir, "interrupted")?;
    frame.emit_line(Role::Tool, "interrupted")
}

struct WatchedDecision<'a> {
    inner: &'a dyn DecisionModel,
    low_shell: &'a Mutex<bool>,
}

impl DecisionModel for WatchedDecision<'_> {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let low_shell = self.low_shell;
        let fut = self.inner.evaluate(req);
        Box::pin(async move {
            let resp = fut.await?;
            if let Some(answer) = resp.answers.get("action") {
                if answer.top() == Some("shell") && answer.confidence < 0.7 {
                    *low_shell.lock().expect("low shell") = true;
                }
            }
            Ok(resp)
        })
    }
}

struct OfflineTurn {
    dir: PathBuf,
    calls: Mutex<usize>,
}

impl OfflineTurn {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            calls: Mutex::new(0),
        }
    }
}

impl DecisionModel for OfflineTurn {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let message = req
            .state
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut calls = self.calls.lock().expect("offline turn");
        let label = if *calls == 0 && contained_file(&self.dir, &message) {
            "read"
        } else if *calls == 0 {
            "search"
        } else {
            "stop"
        };
        *calls += 1;
        drop(calls);
        let label = label.to_string();
        Box::pin(async move {
            let mut answers = std::collections::BTreeMap::new();
            answers.insert(
                "action".into(),
                Answer {
                    probabilities: std::collections::BTreeMap::from([(label, 0.9)]),
                    confidence: 0.9,
                },
            );
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

impl GenerativeModel for OfflineTurn {
    fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        let line = req
            .context
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Box::pin(async move {
            Ok(GenResponse {
                output: GenOutput::Text(line),
                usage: Usage::default(),
            })
        })
    }
}

fn run_turn<W: Write>(
    dir: &Path,
    line: &str,
    input: &mut impl std::io::BufRead,
    model: &dyn DecisionModel,
    writer: &dyn GenerativeModel,
    frame: &mut Frame<'_, W>,
) -> Result<(), String> {
    let system = system_prompt(dir);
    let runtime = tokio::runtime::Runtime::new().map_err(|err| err.to_string())?;
    let low_shell = Mutex::new(false);
    let watched = WatchedDecision {
        inner: model,
        low_shell: &low_shell,
    };
    let steps = runtime
        .block_on(choose(&watched, writer, line, &system))
        .map_err(|err| err.to_string())?;
    for step in steps {
        match step.action {
            Action::Read => {
                let effect = FileRead::new(dir).map_err(|err| err.to_string())?;
                let value = runtime
                    .block_on(effect.run(json!({"path": line})))
                    .map_err(|err| err.to_string())?;
                let path = value
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "read has no path".to_string())?;
                let bytes = value
                    .get("bytes")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "read has no bytes".to_string())?;
                frame.emit_line(Role::Tool, &format!("{path} {bytes}"))?;
            }
            Action::Search => {
                let effect = Search::new(dir).map_err(|err| err.to_string())?;
                let value = runtime
                    .block_on(effect.run(json!({"query": line})))
                    .map_err(|err| err.to_string())?;
                let paths = value
                    .get("paths")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "search has no paths".to_string())?;
                let found = paths
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                frame.emit_line(Role::Tool, &found)?;
            }
            Action::Reply => {
                let text = match step.write {
                    Some(GenOutput::Text(text)) => text,
                    _ => return Err("reply has no text".into()),
                };
                frame.emit_line(Role::Reply, &text)?;
            }
            Action::Edit => {
                let value = match step.write {
                    Some(GenOutput::Json(value)) => value,
                    _ => return Err("edit has no patch".into()),
                };
                let path = value
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "edit has no path".to_string())?;
                let patch = value
                    .get("patch")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "edit has no patch".to_string())?;
                write_edit(dir, path, patch)?;
                frame.emit_line(Role::Tool, path)?;
            }
            Action::Stop => {}
            Action::Shell => {
                confirm_shell(dir, line, &system, &runtime, input, writer, frame)?;
            }
        }
    }
    if *low_shell.lock().expect("low shell") {
        frame.emit_line(Role::Tool, "skip")?;
    }
    Ok(())
}

fn confirm_shell<W: Write>(
    dir: &Path,
    line: &str,
    system: &str,
    runtime: &tokio::runtime::Runtime,
    input: &mut impl std::io::BufRead,
    writer: &dyn GenerativeModel,
    frame: &mut Frame<'_, W>,
) -> Result<(), String> {
    let spec = OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["argv"],
        "properties": {
            "argv": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1
            }
        }
    })));
    let req = GenRequest {
        task: "Return an argv.".into(),
        context: json!({
            "message": line,
            "system": system,
        }),
        output: spec.clone(),
        max_tokens: 1024,
    };
    let resp = runtime
        .block_on(writer.complete(req))
        .map_err(|err| err.to_string())?;
    validate_output(&spec, &resp.output).map_err(|err| err.to_string())?;
    let GenOutput::Json(value) = resp.output else {
        return Err("shell has no argv".into());
    };
    let argv = value
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| "shell has no argv".to_string())?;
    let args = argv
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| "shell has no argv".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(program) = args.first() else {
        return Err("shell has no argv".into());
    };
    if !Path::new(program).is_absolute() {
        return Err(format!(
            "shell: program `{program}` is not an absolute path"
        ));
    }
    let shown = args.join(" ");
    frame.emit_line(Role::Tool, &shown)?;
    append_line(dir, &format!("shell {shown}"))?;
    frame.emit_prompt("yes? ", "yes? ")?;
    let answer = read_one(input)?;
    if answer.as_deref() != Some("yes") {
        frame.emit_line(Role::Tool, "skipped")?;
        append_line(dir, "skipped")?;
        return Ok(());
    }
    let effect = Shell::new(program, &args[1..])
        .map_err(|err| err.to_string())?
        .cwd(dir)
        .map_err(|err| err.to_string())?
        .grant();
    let value = runtime
        .block_on(effect.run(json!({})))
        .map_err(|err| err.to_string())?;
    let stdout = value.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stored = if stdout.is_empty() {
        let code = value.get("status").and_then(Value::as_i64).unwrap_or(0);
        format!("status {code}")
    } else {
        stdout.trim_end_matches('\n').to_string()
    };
    frame.emit_line(Role::Tool, &stored)?;
    append_line(dir, &stored)
}

fn is_contained(path: &Path) -> bool {
    let mut any = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => any = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    any
}

fn contained_file(dir: &Path, line: &str) -> bool {
    let path = Path::new(line);
    is_contained(path) && dir.join(path).is_file()
}

fn write_edit(dir: &Path, rel: &str, patch: &str) -> Result<(), String> {
    let path = Path::new(rel);
    if !is_contained(path) {
        return Err(format!("path `{rel}` escapes the directory"));
    }
    let root = std::fs::canonicalize(dir).map_err(|err| err.to_string())?;
    let full = dir.join(path);
    if std::fs::symlink_metadata(&full)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!("path `{rel}` escapes the directory"));
    }
    let parent = full.parent().unwrap_or(dir);
    let mut existing = parent;
    while !existing.exists() {
        existing = existing.parent().unwrap_or(dir);
    }
    let canonical_existing = std::fs::canonicalize(existing).map_err(|err| err.to_string())?;
    if !canonical_existing.starts_with(&root) {
        return Err(format!("path `{rel}` escapes the directory"));
    }
    if parent != existing {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("creating {}: {err}", parent.display()))?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(|err| err.to_string())?;
        if !canonical_parent.starts_with(&root) {
            return Err(format!("path `{rel}` escapes the directory"));
        }
    }
    std::fs::write(&full, patch).map_err(|err| format!("writing {}: {err}", full.display()))?;
    let canonical_file = std::fs::canonicalize(&full).map_err(|err| err.to_string())?;
    if !canonical_file.starts_with(&root) {
        return Err(format!("path `{rel}` escapes the directory"));
    }
    Ok(())
}

fn write_prompt(output: &mut impl std::io::Write, text: &str) -> Result<(), String> {
    output
        .write_all(text.as_bytes())
        .map_err(|err| err.to_string())?;
    output.flush().map_err(|err| err.to_string())
}

fn append_line(dir: &Path, line: &str) -> Result<(), String> {
    let path = newest_session(dir)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    let entries = parse_entries(&path, &raw)?;
    let parent = entries
        .last()
        .map(|entry| entry.id.clone())
        .ok_or_else(|| "no session".to_string())?;
    let entry = Entry {
        id: next_numeric_id(&entries).to_string(),
        parent: Some(parent),
        text: format!("{line}\n"),
    };
    append_entries(&path, &raw, &[entry])
}

fn continue_session(dir: &Path) -> Result<String, String> {
    let path = newest_session(dir)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    let mut entries = parse_entries(&path, &raw)?;
    let added = compact(&entries)?;
    if !added.is_empty() {
        append_entries(&path, &raw, &added)?;
        entries.extend(added);
    }
    let branch = active_branch(&entries)?;
    Ok(branch.into_iter().map(|entry| entry.text).collect())
}

fn parse_entries(path: &Path, raw: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let entry = serde_json::from_str::<Entry>(line)
            .map_err(|err| format!("{}:{}: {err}", path.display(), index + 1))?;
        entries.push(entry);
    }
    Ok(entries)
}

fn append_entries(path: &Path, raw: &str, added: &[Entry]) -> Result<(), String> {
    let mut extra = String::new();
    if !raw.is_empty() && !raw.ends_with('\n') {
        extra.push('\n');
    }
    for entry in added {
        let line = serde_json::to_string(entry).map_err(|err| err.to_string())?;
        extra.push_str(&line);
        extra.push('\n');
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|err| format!("writing {}: {err}", path.display()))?;
    std::io::Write::write_all(&mut file, extra.as_bytes())
        .map_err(|err| format!("writing {}: {err}", path.display()))
}

fn sessions_dir(dir: &Path) -> PathBuf {
    dir.join(".seaotter").join("sessions")
}

fn session_file_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{n}")
}

fn newest_session(dir: &Path) -> Result<PathBuf, String> {
    let sessions = sessions_dir(dir);
    let entries = std::fs::read_dir(&sessions).map_err(|_| "no session".to_string())?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map_err(|err| err.to_string())?;
        let replace = match &best {
            Some((time, previous)) => modified > *time || (modified == *time && path > *previous),
            None => true,
        };
        if replace {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
        .ok_or_else(|| "no session".into())
}

fn active_branch(entries: &[Entry]) -> Result<Vec<Entry>, String> {
    let tip = entries.last().ok_or_else(|| "no session".to_string())?;
    let mut chain = Vec::new();
    let mut current = Some(tip.id.clone());
    while let Some(id) = current {
        if chain.len() > entries.len() {
            return Err("session cycle".into());
        }
        let entry = entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("missing session entry `{id}`"))?;
        current = entry.parent.clone();
        chain.push(entry.clone());
    }
    chain.reverse();
    Ok(chain)
}

fn compact(entries: &[Entry]) -> Result<Vec<Entry>, String> {
    let branch = active_branch(entries)?;
    if branch.len() < 2 || branch_chars(&branch) <= CONTEXT_LIMIT {
        return Ok(Vec::new());
    }
    let recent_count = RECENT_TURNS.min(branch.len() - 1);
    let split = branch.len() - recent_count;
    let older = &branch[..split];
    let recent = &branch[split..];
    if older.len() == 1 && branch_chars(older) <= SUMMARY_MAX {
        return Ok(Vec::new());
    }
    let mut next = next_numeric_id(entries);
    let summary_id = next.to_string();
    let mut added = vec![Entry {
        id: summary_id.clone(),
        parent: older[0].parent.clone(),
        text: summary_text(older),
    }];
    let mut parent = summary_id;
    for turn in recent {
        next += 1;
        let id = next.to_string();
        added.push(Entry {
            id: id.clone(),
            parent: Some(parent),
            text: turn.text.clone(),
        });
        parent = id;
    }
    Ok(added)
}

fn branch_chars(entries: &[Entry]) -> usize {
    entries.iter().map(|entry| entry.text.chars().count()).sum()
}

fn summary_text(older: &[Entry]) -> String {
    let joined: String = older.iter().map(|entry| entry.text.as_str()).collect();
    joined.chars().take(SUMMARY_MAX).collect()
}

fn next_numeric_id(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .filter_map(|entry| entry.id.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

fn ensure_workspace(root: &Path) -> Result<(), String> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)
        .map_err(|err| format!("creating {}: {err}", workspace.display()))
}

fn parse_workflow(args: &[String]) -> Result<(&str, &'static str, &[String]), String> {
    if args.first().map(String::as_str) != Some("--workflow") {
        return Err("--workflow is required".into());
    }
    let name = args.get(1).ok_or("--workflow needs a name")?;
    let bin = match name.as_str() {
        "examples/ticket-router" => "ticket-router",
        "examples/gated-generate" => "gated-generate",
        "examples/effects" => "effects",
        other => return Err(format!("unknown workflow `{other}`")),
    };
    Ok((name.as_str(), bin, &args[2..]))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;

    use harness_core::{
        Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, GenOutput, GenRequest,
        GenResponse, GenerativeModel, HarnessError, Usage,
    };
    use serde_json::json;

    use super::{
        Entry, Request, Role, TranscriptLine, active_branch, continue_session, ensure_workspace,
        json_events, layout, parse, print_reply, read_prompt, serve_rpc, session, start_session,
        system_prompt,
    };

    struct ScriptedDecision {
        steps: Mutex<VecDeque<(&'static str, f64)>>,
        seen: Mutex<Vec<DecisionRequest>>,
    }

    impl ScriptedDecision {
        fn new(steps: &[(&'static str, f64)]) -> Self {
            Self {
                steps: Mutex::new(steps.iter().copied().collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl DecisionModel for ScriptedDecision {
        fn evaluate(
            &self,
            req: DecisionRequest,
        ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
            let (top, confidence) = self.steps.lock().unwrap().pop_front().unwrap();
            self.seen.lock().unwrap().push(req);
            Box::pin(async move {
                let mut answers = BTreeMap::new();
                answers.insert(
                    "action".into(),
                    Answer {
                        probabilities: BTreeMap::from([(top.to_owned(), confidence)]),
                        confidence,
                    },
                );
                Ok(DecisionResponse {
                    answers,
                    usage: Usage::default(),
                })
            })
        }
    }

    struct ScriptedWriter {
        outputs: Mutex<VecDeque<GenOutput>>,
        calls: Mutex<usize>,
    }

    impl ScriptedWriter {
        fn new(outputs: Vec<GenOutput>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into()),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl GenerativeModel for ScriptedWriter {
        fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
            let _ = req;
            *self.calls.lock().unwrap() += 1;
            let output = self.outputs.lock().unwrap().pop_front().unwrap();
            Box::pin(async move {
                Ok(GenResponse {
                    output,
                    usage: Usage::default(),
                })
            })
        }
    }

    fn stop_model() -> (ScriptedDecision, ScriptedWriter) {
        (
            ScriptedDecision::new(&[("stop", 0.9)]),
            ScriptedWriter::new(vec![]),
        )
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "seaotter-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(str::to_string).collect()
    }

    #[test]
    fn ticket_router() {
        let args = args(&["--workflow", "examples/ticket-router"]);
        let Request::Workflow(workflow, bin, rest) = parse(&args).unwrap() else {
            panic!("expected a workflow");
        };
        assert_eq!(workflow, "examples/ticket-router");
        assert_eq!(bin, "ticket-router");
        assert!(rest.is_empty());
    }

    #[test]
    fn gated_generate_forwards_later_args() {
        let args = args(&["--workflow", "examples/gated-generate", "--live", "openai"]);
        let Request::Workflow(workflow, bin, rest) = parse(&args).unwrap() else {
            panic!("expected a workflow");
        };
        assert_eq!(workflow, "examples/gated-generate");
        assert_eq!(bin, "gated-generate");
        assert_eq!(rest, ["--live", "openai"]);
    }

    #[test]
    fn unknown_workflow() {
        let args = args(&["--workflow", "no-such"]);
        let err = parse(&args).unwrap_err();
        assert!(err.contains("no-such"), "{err}");
    }

    #[test]
    fn no_arguments_open_a_session() {
        assert!(matches!(parse(&[]).unwrap(), Request::Session));
    }

    #[test]
    fn an_empty_session_prints_the_directory_and_a_prompt() {
        let dir = std::env::temp_dir().join(format!("seaotter-session-{}", std::process::id()));
        let text = session(&dir);
        assert_eq!(text, format!("{}\n> \n", dir.display()));
    }

    #[test]
    fn agents_md_is_the_system_prompt() {
        let dir = std::env::temp_dir().join(format!("seaotter-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "use rust\n").unwrap();
        std::fs::write(dir.join("SKILL.md"), "skill secret\n").unwrap();
        assert_eq!(system_prompt(&dir), "use rust\n");
        let text = session(&dir);
        assert_eq!(text, format!("{}\nuse rust\n> \n", dir.display()));
        assert!(!text.contains("skill secret"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_parent_agents_file_is_not_read() {
        let parent =
            std::env::temp_dir().join(format!("seaotter-agents-parent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("AGENTS.md"), "parent instructions\n").unwrap();
        let text = session(&child);
        assert_eq!(text, format!("{}\n> \n", child.display()));
        assert!(!text.contains("parent instructions"), "{text}");
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_new_session_is_one_jsonl_line() {
        let dir = std::env::temp_dir().join(format!("seaotter-jsonl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = start_session(&dir).unwrap();
        assert_eq!(text, session(&dir));
        let mut files = std::fs::read_dir(dir.join(".seaotter/sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        let raw = std::fs::read_to_string(files.pop().unwrap()).unwrap();
        let lines = raw
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        let entry: Entry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry.parent, None);
        assert_eq!(entry.text, text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn prompt_entries(dir: &std::path::Path) -> Vec<Entry> {
        let mut files = std::fs::read_dir(dir.join(".seaotter/sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        let raw = std::fs::read_to_string(files.pop().unwrap()).unwrap();
        raw.lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_line_is_echoed_and_an_empty_line_is_not_stored() {
        let dir = temp_dir("prompt");
        let mut output = Vec::new();
        let (model, writer) = stop_model();
        read_prompt(
            &dir,
            std::io::Cursor::new("\nhello\n\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text, format!("{}> \nhello\n> \n> \n", session(&dir)));
        let entries = prompt_entries(&dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "1");
        assert_eq!(entries[0].parent, None);
        assert_eq!(entries[1].id, "2");
        assert_eq!(entries[1].parent.as_deref(), Some("1"));
        assert_eq!(entries[1].text, "hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_of_input_stores_the_line() {
        let dir = temp_dir("prompt-eof");
        let mut output = Vec::new();
        let (model, writer) = stop_model();
        read_prompt(
            &dir,
            std::io::Cursor::new("hello\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text, format!("{}hello\n> \n", session(&dir)));
        let entries = prompt_entries(&dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].id, "2");
        assert_eq!(entries[1].parent.as_deref(), Some("1"));
        assert_eq!(entries[1].text, "hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_calls_choose_and_reads_one_line() {
        let dir = temp_dir("read");
        std::fs::write(dir.join("AGENTS.md"), "be brief\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "hello\n").unwrap();
        let model = ScriptedDecision::new(&[("read", 0.9), ("stop", 0.9)]);
        let writer = ScriptedWriter::new(vec![]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("notes.txt\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("notes.txt 6\n"), "{text}");
        assert!(text.ends_with("> \n"), "{text}");
        let seen = model.seen.lock().unwrap();
        assert!(!seen.is_empty());
        for req in seen.iter() {
            assert_eq!(
                req.state.get("message").and_then(|value| value.as_str()),
                Some("notes.txt")
            );
            assert_eq!(
                req.state.get("system").and_then(|value| value.as_str()),
                Some("be brief\n")
            );
        }
        let entries = prompt_entries(&dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].text, "notes.txt\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_prints_one_result_line() {
        let dir = temp_dir("search");
        std::fs::write(dir.join("notes.txt"), "needle\n").unwrap();
        let model = ScriptedDecision::new(&[("search", 0.9), ("stop", 0.9)]);
        let writer = ScriptedWriter::new(vec![]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("needle\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        let prefix = format!("{}needle\n", session(&dir));
        let rest = text.strip_prefix(&prefix).expect(&text);
        let (result, prompt) = rest.split_once("> \n").expect(rest);
        assert_eq!(prompt, "");
        assert_eq!(result.lines().count(), 1, "{result}");
        assert!(result.contains("notes.txt"), "{result}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reply_prints_the_system_two_text() {
        let dir = temp_dir("reply");
        let model = ScriptedDecision::new(&[("reply", 0.9)]);
        let writer = ScriptedWriter::new(vec![GenOutput::Text("fixed".into())]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("hello\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text, format!("{}hello\nfixed\n> \n", session(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_writes_inside_the_directory_and_prints_the_path() {
        let dir = temp_dir("edit");
        let model = ScriptedDecision::new(&[("edit", 0.9), ("stop", 0.9)]);
        let writer = ScriptedWriter::new(vec![GenOutput::Json(
            json!({"path": "out.txt", "patch": "hello\n"}),
        )]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("go\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text, format!("{}go\nout.txt\n> \n", session(&dir)));
        assert_eq!(
            std::fs::read_to_string(dir.join("out.txt")).unwrap(),
            "hello\n"
        );
        let _ = std::fs::remove_dir_all(&dir);

        let dir = temp_dir("edit-outside");
        let outside_name = format!("seaotter-outside-{}.txt", std::process::id());
        let outside = dir.parent().unwrap().join(&outside_name);
        let _ = std::fs::remove_file(&outside);
        let model = ScriptedDecision::new(&[("edit", 0.9), ("stop", 0.9)]);
        let writer = ScriptedWriter::new(vec![GenOutput::Json(
            json!({"path": format!("../{outside_name}"), "patch": "no\n"}),
        )]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("go\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        assert!(!outside.exists());
        assert!(!dir.join(&outside_name).exists());
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text, format!("{}go\n> \n", session(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(outside);
    }

    fn echo_argv() -> GenOutput {
        GenOutput::Json(json!({"argv": ["/bin/echo", "hello"]}))
    }

    #[test]
    fn yes_spawns_the_shell_and_stores_its_result() {
        let dir = temp_dir("shell-yes");
        let model = ScriptedDecision::new(&[("shell", 0.9), ("stop", 0.9)]);
        let writer = ScriptedWriter::new(vec![echo_argv()]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("go\nyes\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("/bin/echo hello"), "{text}");
        assert!(text.contains("hello\n"), "{text}");
        assert_eq!(writer.calls(), 1);
        let entries = prompt_entries(&dir);
        assert!(
            entries
                .iter()
                .any(|entry| entry.text == "shell /bin/echo hello\n")
        );
        assert!(entries.iter().any(|entry| entry.text == "hello\n"));
        let shell = entries
            .iter()
            .position(|entry| entry.text == "shell /bin/echo hello\n")
            .unwrap();
        assert_eq!(entries[shell + 1].text, "hello\n");
        assert!(entries.iter().all(|entry| entry.text != "yes\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_skips_the_shell_without_spawning() {
        let dir = temp_dir("shell-no");
        let model = ScriptedDecision::new(&[("shell", 0.9), ("stop", 0.9)]);
        let writer = ScriptedWriter::new(vec![echo_argv()]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("go\nno\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("skipped"), "{text}");
        assert!(!text.contains("\nhello\n"), "{text}");
        let entries = prompt_entries(&dir);
        let shell = entries
            .iter()
            .position(|entry| entry.text == "shell /bin/echo hello\n")
            .unwrap();
        assert_eq!(entries[shell + 1].text, "skipped\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_low_confidence_shell_prints_skip() {
        let dir = temp_dir("shell-low");
        let model = ScriptedDecision::new(&[("shell", 0.2)]);
        let writer = ScriptedWriter::new(vec![]);
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("go\n/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("skip\n"), "{text}");
        assert_eq!(writer.calls(), 0);
        let entries = prompt_entries(&dir);
        assert!(
            entries
                .iter()
                .all(|entry| !entry.text.starts_with("shell "))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shell_without_a_result_is_interrupted_on_the_next_open() {
        let dir = temp_dir("shell-crash");
        let marker = dir.join("marker");
        let sessions = dir.join(".seaotter/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let crashed = sessions.join("crashed.jsonl");
        let shell = format!("shell /usr/bin/touch {}\n", marker.display());
        let lines = [
            Entry {
                id: "1".into(),
                parent: None,
                text: "session\n".into(),
            },
            Entry {
                id: "2".into(),
                parent: Some("1".into()),
                text: shell,
            },
        ];
        let raw = lines
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&crashed, &raw).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&crashed)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        let (model, writer) = stop_model();
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("interrupted\n"), "{text}");
        assert!(!marker.exists());
        let saved = std::fs::read_to_string(&crashed).unwrap();
        let entries: Vec<Entry> = saved
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries.last().unwrap().text, "interrupted\n");
        assert_eq!(entries.last().unwrap().parent.as_deref(), Some("2"));
        let (model, writer) = stop_model();
        let mut output = Vec::new();
        read_prompt(
            &dir,
            std::io::Cursor::new("/quit\n"),
            &mut output,
            &model,
            &writer,
            false,
        )
        .unwrap();
        let again = std::fs::read_to_string(&crashed).unwrap();
        assert_eq!(again, saved);
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn continue_parses_and_a_missing_session_fails() {
        assert!(matches!(
            parse(&args(&["--continue"])).unwrap(),
            Request::Continue
        ));
        let dir = std::env::temp_dir().join(format!("seaotter-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = continue_session(&dir).unwrap_err();
        assert!(err.contains("session"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fork_leaves_the_sibling_off_the_active_branch() {
        let dir = std::env::temp_dir().join(format!("seaotter-fork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sessions = dir.join(".seaotter/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let older = sessions.join("older.jsonl");
        let newer = sessions.join("newer.jsonl");
        std::fs::write(
            &older,
            "{\"id\":\"1\",\"parent\":null,\"text\":\"old session\\n\"}\n",
        )
        .unwrap();
        std::fs::write(
            &newer,
            concat!(
                "{\"id\":\"1\",\"parent\":null,\"text\":\"root\\n\"}\n",
                "{\"id\":\"2\",\"parent\":\"1\",\"text\":\"sibling\\n\"}\n",
                "{\"id\":\"3\",\"parent\":\"1\",\"text\":\"tip\\n\"}\n",
            ),
        )
        .unwrap();
        let epoch = std::time::SystemTime::UNIX_EPOCH;
        std::fs::File::options()
            .write(true)
            .open(&older)
            .unwrap()
            .set_modified(epoch + std::time::Duration::from_secs(1))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&newer)
            .unwrap()
            .set_modified(epoch + std::time::Duration::from_secs(2))
            .unwrap();
        let text = continue_session(&dir).unwrap();
        assert_eq!(text, "root\ntip\n");
        assert!(!text.contains("sibling"), "{text}");
        assert!(!text.contains("old session"), "{text}");
        let entries = vec![
            Entry {
                id: "1".into(),
                parent: None,
                text: "root\n".into(),
            },
            Entry {
                id: "2".into(),
                parent: Some("1".into()),
                text: "sibling\n".into(),
            },
            Entry {
                id: "3".into(),
                parent: Some("1".into()),
                text: "tip\n".into(),
            },
        ];
        let branch = active_branch(&entries).unwrap();
        assert_eq!(
            branch
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "3"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn entry_line(id: &str, parent: Option<&str>, text: &str) -> String {
        let entry = Entry {
            id: id.into(),
            parent: parent.map(str::to_string),
            text: text.into(),
        };
        format!("{}\n", serde_json::to_string(&entry).unwrap())
    }

    fn write_session(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let sessions = dir.join(".seaotter/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_short_branch_is_unchanged() {
        let dir = std::env::temp_dir().join(format!("seaotter-short-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body = format!(
            "{}{}",
            entry_line("1", None, "root\n"),
            entry_line("2", Some("1"), "tip\n"),
        );
        let path = write_session(&dir, "session.jsonl", &body);
        let text = continue_session(&dir).unwrap();
        assert_eq!(text, "root\ntip\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_branch_keeps_recent_turns_and_the_original_lines() {
        let dir = std::env::temp_dir().join(format!("seaotter-compact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let older_a = "a".repeat(200);
        let older_b = format!("{}PAST-CUT", "b".repeat(100));
        let recent_a = format!("{}RECENT-A", "r".repeat(400));
        let recent_b = format!("{}RECENT-B", "s".repeat(400));
        let body = format!(
            "{}{}{}{}{}",
            entry_line("1", None, &older_a),
            entry_line("2", Some("1"), &older_b),
            entry_line("3", Some("1"), "SIBLING\n"),
            entry_line("4", Some("2"), &recent_a),
            entry_line("5", Some("4"), &recent_b),
        );
        let path = write_session(&dir, "session.jsonl", &body);
        let text = continue_session(&dir).unwrap();
        assert!(text.contains("RECENT-A"), "{text}");
        assert!(text.contains("RECENT-B"), "{text}");
        assert!(!text.contains("PAST-CUT"), "{text}");
        assert!(!text.contains("SIBLING"), "{text}");
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(stored.starts_with(&body), "{stored}");
        let added = stored[body.len()..]
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<Entry>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(added.len(), 3, "{added:?}");
        assert!(added[0].text.chars().count() <= 256, "{}", added[0].text);
        assert!(!added[0].text.contains("PAST-CUT"));
        assert_eq!(added[1].text, recent_a);
        assert_eq!(added[2].text, recent_b);
        assert_eq!(added[2].parent.as_deref(), Some(added[1].id.as_str()));
        let again = std::fs::read_to_string(&path).unwrap();
        let second = continue_session(&dir).unwrap();
        assert_eq!(second, text);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_oversized_entry_is_not_rewritten() {
        let dir = std::env::temp_dir().join(format!("seaotter-one-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "x".repeat(2000);
        let body = entry_line("1", None, &text);
        let path = write_session(&dir, "session.jsonl", &body);
        let printed = continue_session(&dir).unwrap();
        assert_eq!(printed, text);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workflow_without_a_name_still_fails() {
        let err = parse(&args(&["--workflow"])).unwrap_err();
        assert!(err.contains("name"), "{err}");
    }

    #[test]
    fn effects_workflow() {
        let args = args(&["--workflow", "examples/effects"]);
        let Request::Workflow(workflow, bin, rest) = parse(&args).unwrap() else {
            panic!("expected a workflow");
        };
        assert_eq!(workflow, "examples/effects");
        assert_eq!(bin, "effects");
        assert!(rest.is_empty());
    }

    #[test]
    fn print_writes_the_message_and_needs_one() {
        let printed = args(&["--print", "hello"]);
        let Request::Print(message) = parse(&printed).unwrap() else {
            panic!("expected print");
        };
        assert_eq!(message, "hello");
        assert_eq!(print_reply(message), "hello\n");
        assert!(!print_reply(message).contains('\u{1b}'));
        let err = parse(&args(&["--print"])).unwrap_err();
        assert!(err.contains("message"), "{err}");
        let err = parse(&args(&["--print", "hello", "extra"])).unwrap_err();
        assert!(err.contains("message"), "{err}");
    }

    fn line(role: Role, text: &str) -> TranscriptLine {
        TranscriptLine {
            role,
            text: text.to_string(),
        }
    }

    #[test]
    fn the_transcript_keeps_the_prompt_on_the_last_row() {
        let lines = vec![
            line(Role::Person, "fix it"),
            line(Role::Tool, "notes.txt 6"),
            line(Role::Reply, "fixed"),
        ];
        let screen = layout(&[], &lines, 6, "> ");
        let above = screen
            .rows
            .iter()
            .take(screen.rows.len() - 1)
            .filter(|row| !row.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            above,
            vec![
                "person fix it".to_string(),
                "tool notes.txt 6".to_string(),
                "reply fixed".to_string(),
            ]
        );
        assert_eq!(screen.rows.last().map(String::as_str), Some("> "));
        assert_eq!(screen.cursor_row, screen.rows.len() - 1);
        assert_eq!(screen.cursor_col, 2);
        assert_eq!(screen.rows.len(), 6);
    }

    #[test]
    fn a_long_transcript_keeps_the_newest_lines_above_the_prompt() {
        let lines = (1..=5)
            .map(|n| line(Role::Person, &format!("n{n}")))
            .collect::<Vec<_>>();
        let screen = layout(&[], &lines, 3, "> ");
        assert_eq!(screen.rows.len(), 3);
        assert_eq!(screen.rows[0], "person n4");
        assert_eq!(screen.rows[1], "person n5");
        assert_eq!(screen.rows[2], "> ");
        assert_eq!(screen.cursor_row, 2);
        assert_eq!(screen.cursor_col, 2);
    }

    #[test]
    fn the_shell_question_is_the_bottom_prompt() {
        let screen = layout(&[], &[], 4, "yes? ");
        assert_eq!(screen.rows.last().map(String::as_str), Some("yes? "));
        assert_eq!(screen.cursor_row, 3);
        assert_eq!(screen.cursor_col, 5);
    }

    #[test]
    fn json_mode_writes_three_events() {
        let json = args(&["--mode", "json", "hello"]);
        let Request::Json(message) = parse(&json).unwrap() else {
            panic!("expected json");
        };
        assert_eq!(message, "hello");
        let stream = json_events(message);
        let lines: Vec<_> = stream.lines().collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        let decision: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let effect: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        let reply: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(decision["kind"], "decision");
        assert_eq!(decision["action"], "read");
        assert_eq!(effect["kind"], "effect");
        assert_eq!(effect["name"], "read");
        assert!(effect.get("output").is_none());
        assert_eq!(reply["kind"], "reply");
        assert_eq!(reply["text"], "hello");
        let err = parse(&args(&["--mode"])).unwrap_err();
        assert!(err.contains("json"), "{err}");
        assert!(matches!(
            parse(&args(&["--mode", "rpc"])).unwrap(),
            Request::Rpc
        ));
        let err = parse(&args(&["--mode", "rpc", "extra"])).unwrap_err();
        assert!(err.contains("message"), "{err}");
        let err = parse(&args(&["--mode", "other"])).unwrap_err();
        assert!(err.contains("other"), "{err}");
    }

    fn rpc_lines(input: &str) -> Vec<serde_json::Value> {
        serve_rpc(input)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn rpc_prompt_writes_events_and_a_response() {
        let lines = rpc_lines(r#"{"command":"prompt","message":"hello"}"#);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(lines[0]["kind"], "decision");
        assert_eq!(lines[0]["action"], "read");
        assert_eq!(lines[1]["kind"], "effect");
        assert_eq!(lines[1]["name"], "read");
        assert_eq!(lines[2]["kind"], "reply");
        assert_eq!(lines[2]["text"], "hello");
        assert_eq!(lines[3]["kind"], "response");
        assert_eq!(lines[3]["command"], "prompt");
        let missing = rpc_lines(r#"{"command":"prompt"}"#);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert_eq!(missing[0]["error"], "message");
        assert!(missing[0].get("text").is_none());
    }

    #[test]
    fn rpc_continue_repeats_the_prompt_or_reports_none() {
        let lines = rpc_lines(
            "{\"command\":\"prompt\",\"message\":\"hello\"}\n{\"command\":\"continue\"}\n",
        );
        assert_eq!(lines.len(), 8, "{lines:?}");
        assert_eq!(lines[6]["kind"], "reply");
        assert_eq!(lines[6]["text"], "hello");
        assert_eq!(lines[7]["kind"], "response");
        assert_eq!(lines[7]["command"], "continue");
        let missing = rpc_lines(
            "{\"command\":\"continue\"}\n{\"command\":\"prompt\",\"message\":\"later\"}\n",
        );
        assert_eq!(missing[0]["error"], "no prompt");
        assert!(
            missing
                .iter()
                .take(1)
                .all(|line| line["kind"] != "decision")
        );
        assert_eq!(missing[1]["kind"], "decision");
        assert_eq!(missing.last().unwrap()["command"], "prompt");
    }

    #[test]
    fn rpc_abort_stops_before_the_next_command() {
        let lines =
            rpc_lines("{\"command\":\"abort\"}\n{\"command\":\"prompt\",\"message\":\"later\"}\n");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["kind"], "response");
        assert_eq!(lines[0]["command"], "abort");
        assert!(
            !serve_rpc("{\"command\":\"abort\"}\n{\"command\":\"prompt\",\"message\":\"later\"}\n")
                .contains("later")
        );
    }

    #[test]
    fn rpc_does_not_write_a_session() {
        let dir = std::env::temp_dir().join(format!("seaotter-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = serve_rpc(r#"{"command":"prompt","message":"hello"}"#);
        assert!(!dir.join(".seaotter").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn print_and_json_do_not_write_a_session() {
        let dir = std::env::temp_dir().join(format!("seaotter-print-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(print_reply("hello"), "hello\n");
        assert!(json_events("hello").contains("\"kind\":\"reply\""));
        assert!(!dir.join(".seaotter").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn health_does_not_select_a_binary() {
        let args = args(&["--health"]);
        assert!(matches!(parse(&args).unwrap(), Request::Health));
    }

    #[test]
    fn workspace_is_created_without_a_log() {
        let root = std::env::temp_dir().join(format!("seaotter-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        ensure_workspace(&root).unwrap();
        assert!(root.join("workspace").is_dir());
        assert!(!root.join("log").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
