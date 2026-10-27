use std::path::Path;
use std::process::{Command, ExitCode};

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
            print!("{}", session(&dir));
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
    Workflow(&'a str, &'static str, &'a [String]),
}

fn parse(args: &[String]) -> Result<Request<'_>, String> {
    if args.is_empty() {
        return Ok(Request::Session);
    }
    if args.len() == 1 && args[0] == "--health" {
        return Ok(Request::Health);
    }
    let (workflow, bin, rest) = parse_workflow(args)?;
    Ok(Request::Workflow(workflow, bin, rest))
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
    use super::{Request, ensure_workspace, parse, session, system_prompt};

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
