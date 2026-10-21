use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (bin, rest) = match parse_workflow(&args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    match Command::new(bin).args(rest).status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(err) => {
            eprintln!("running {bin}: {err}");
            ExitCode::FAILURE
        }
    }
}

fn parse_workflow(args: &[String]) -> Result<(&'static str, &[String]), String> {
    if args.first().map(String::as_str) != Some("--workflow") {
        return Err("--workflow is required".into());
    }
    let name = args.get(1).ok_or("--workflow needs a name")?;
    let bin = match name.as_str() {
        "examples/ticket-router" => "ticket-router",
        "examples/gated-generate" => "gated-generate",
        other => return Err(format!("unknown workflow `{other}`")),
    };
    Ok((bin, &args[2..]))
}

#[cfg(test)]
mod tests {
    use super::parse_workflow;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(str::to_string).collect()
    }

    #[test]
    fn ticket_router() {
        let args = args(&["--workflow", "examples/ticket-router"]);
        let (bin, rest) = parse_workflow(&args).unwrap();
        assert_eq!(bin, "ticket-router");
        assert!(rest.is_empty());
    }

    #[test]
    fn gated_generate_forwards_later_args() {
        let args = args(&["--workflow", "examples/gated-generate", "--live", "openai"]);
        let (bin, rest) = parse_workflow(&args).unwrap();
        assert_eq!(bin, "gated-generate");
        assert_eq!(rest, ["--live", "openai"]);
    }

    #[test]
    fn unknown_workflow() {
        let args = args(&["--workflow", "no-such"]);
        let err = parse_workflow(&args).unwrap_err();
        assert!(err.contains("no-such"), "{err}");
    }

    #[test]
    fn missing_flag() {
        let err = parse_workflow(&[]).unwrap_err();
        assert!(err.contains("--workflow"), "{err}");
    }
}
