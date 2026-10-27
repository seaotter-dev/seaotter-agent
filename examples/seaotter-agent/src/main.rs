use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

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
            match start_session(&dir) {
                Ok(text) => print!("{text}"),
                Err(err) => {
                    eprintln!("{err}");
                    return ExitCode::FAILURE;
                }
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

fn continue_session(dir: &Path) -> Result<String, String> {
    let path = newest_session(dir)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading {}: {err}", path.display()))?;
    let mut entries = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let entry = serde_json::from_str::<Entry>(line)
            .map_err(|err| format!("{}:{}: {err}", path.display(), index + 1))?;
        entries.push(entry);
    }
    let branch = active_branch(&entries)?;
    Ok(branch.into_iter().map(|entry| entry.text).collect())
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
    use super::{
        Entry, Request, active_branch, continue_session, ensure_workspace, parse, session,
        start_session, system_prompt,
    };

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
