use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (workflow, bin, rest) = match parse(&args) {
        Ok(Request::Health) => return ExitCode::SUCCESS,
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
    if code == 0 {
        let user_id = std::env::var("USER_ID").unwrap_or_default();
        if let Err(err) = record_run(Path::new("/data"), workflow, &user_id) {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::from(code)
}

#[derive(Debug)]
enum Request<'a> {
    Health,
    Workflow(&'a str, &'static str, &'a [String]),
}

fn parse(args: &[String]) -> Result<Request<'_>, String> {
    if args.len() == 1 && args[0] == "--health" {
        return Ok(Request::Health);
    }
    let (workflow, bin, rest) = parse_workflow(args)?;
    Ok(Request::Workflow(workflow, bin, rest))
}

fn record_run(root: &Path, workflow: &str, user_id: &str) -> Result<(), String> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)
        .map_err(|err| format!("creating {}: {err}", workspace.display()))?;
    let line = format!(
        "{{\"workflow\":{},\"user_id\":{}}}\n",
        json_string(workflow),
        json_string(user_id)
    );
    let path = root.join("log");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("opening {}: {err}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|err| format!("writing {}: {err}", path.display()))
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
    use super::{Request, parse, record_run};

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
    fn missing_flag() {
        let err = parse(&[]).unwrap_err();
        assert!(err.contains("--workflow"), "{err}");
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
    fn health_does_not_select_a_binary() {
        let args = args(&["--health"]);
        assert!(matches!(parse(&args).unwrap(), Request::Health));
    }

    #[test]
    fn record_appends_user_id_and_omits_secrets() {
        let root = std::env::temp_dir().join(format!("seaotter-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        record_run(&root, "examples/ticket-router", "user-7").unwrap();
        record_run(&root, "examples/gated-generate", "user\"7").unwrap();
        let log = std::fs::read_to_string(root.join("log")).unwrap();
        assert!(!log.contains("sk-test"), "{log}");
        let mut lines = log.lines();
        assert_eq!(
            lines.next().unwrap(),
            r#"{"workflow":"examples/ticket-router","user_id":"user-7"}"#
        );
        assert_eq!(
            lines.next().unwrap(),
            r#"{"workflow":"examples/gated-generate","user_id":"user\"7"}"#
        );
        assert!(lines.next().is_none());
        assert!(root.join("workspace").is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
