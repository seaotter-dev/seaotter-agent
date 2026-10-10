use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use harness_core::{BoxFuture, Capability, Effect, HarnessError};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

pub const DEFAULT_NAME: &str = "shell";
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs one fixed command under [`Capability::DangerousShell`]. The program,
/// args, cwd, and environment are set in Rust when the effect is built; `run`
/// ignores its input, so workflow state (and model output in it) can never
/// change what executes. Output is `{"status", "stdout", "stderr"}` for any
/// exit code.
///
/// The program runs directly, without a shell, so there is no expansion,
/// piping, or globbing. The child starts with an empty environment plus the
/// variables added with [`Shell::env`].
#[derive(Debug, Clone)]
pub struct Shell {
    name: String,
    program: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: BTreeMap<OsString, OsString>,
    max_bytes: u64,
    timeout: Duration,
}

impl Shell {
    /// `program` must be an absolute path: with a cleared environment there is
    /// no `PATH` lookup.
    pub fn new<I, S>(program: impl AsRef<Path>, args: I) -> Result<Self, HarnessError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let program = program.as_ref();
        if program.as_os_str().is_empty() {
            return Err(HarnessError::Effect("shell: program is empty".into()));
        }
        if !program.is_absolute() {
            return Err(HarnessError::Effect(format!(
                "shell: program `{}` is not an absolute path",
                program.display()
            )));
        }
        Ok(Self {
            name: DEFAULT_NAME.into(),
            program: program.to_owned(),
            args: args.into_iter().map(|a| a.as_ref().to_owned()).collect(),
            cwd: None,
            env: BTreeMap::new(),
            max_bytes: DEFAULT_MAX_BYTES,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Runs the command in `dir`, which must be an existing directory. It is
    /// canonicalized once here. Without this the child keeps the process cwd.
    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Result<Self, HarnessError> {
        let dir = dir.as_ref();
        let canonical = std::fs::canonicalize(dir)
            .map_err(|e| self.err(format!("cwd {}: {e}", dir.display())))?;
        if !canonical.is_dir() {
            return Err(self.err(format!("cwd {} is not a directory", dir.display())));
        }
        self.cwd = Some(canonical);
        Ok(self)
    }

    /// Adds one variable to the child's otherwise empty environment.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .insert(key.as_ref().to_owned(), value.as_ref().to_owned());
        self
    }

    /// Caps stdout and stderr separately.
    pub fn max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn err(&self, msg: impl std::fmt::Display) -> HarnessError {
        HarnessError::Effect(format!("{}: {msg}", self.name))
    }

    async fn exec(&self) -> Result<Value, HarnessError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| self.err(format!("spawn {}: {e}", self.program.display())))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let max = self.max_bytes;
        let work = async {
            let (out, err) = tokio::try_join!(
                read_capped(stdout, max, "stdout"),
                read_capped(stderr, max, "stderr")
            )?;
            let status = child.wait().await.map_err(|e| format!("wait: {e}"))?;
            Ok::<_, String>((out, err, status))
        };
        let (out, err, status) = match tokio::time::timeout(self.timeout, work).await {
            Ok(Ok(done)) => done,
            Ok(Err(msg)) => {
                let _ = child.kill().await;
                return Err(self.err(msg));
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(self.err(format!("timed out after {:?}", self.timeout)));
            }
        };

        let code = status
            .code()
            .ok_or_else(|| self.err("terminated by a signal"))?;
        let stdout = String::from_utf8(out).map_err(|_| self.err("stdout is not UTF-8 text"))?;
        let stderr = String::from_utf8(err).map_err(|_| self.err("stderr is not UTF-8 text"))?;
        Ok(json!({"status": code, "stdout": stdout, "stderr": stderr}))
    }
}

async fn read_capped(
    stream: impl AsyncRead + Unpin,
    max: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    stream
        .take(max + 1)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| format!("read {label}: {e}"))?;
    if buf.len() as u64 > max {
        return Err(format!("{label} is over the {max} byte limit"));
    }
    Ok(buf)
}

impl Effect for Shell {
    fn name(&self) -> &str {
        &self.name
    }

    fn capability(&self) -> Capability {
        Capability::DangerousShell
    }

    fn run(&self, _input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(self.exec())
    }
}
