use std::path::{Component, Path, PathBuf};

use harness_core::{BoxFuture, Capability, Effect, HarnessError};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

pub const DEFAULT_NAME: &str = "file_read";
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

/// Reads UTF-8 files under a fixed root. Input is `{"path": "<relative>"}`;
/// output is `{"path", "contents", "bytes"}`. Paths with `..`, absolute
/// paths, and symlinks that resolve outside the root are rejected before the
/// file is opened.
#[derive(Debug, Clone)]
pub struct FileRead {
    name: String,
    root: PathBuf,
    max_bytes: u64,
}

impl FileRead {
    /// `root` must be an existing directory. It is canonicalized once here.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, HarnessError> {
        let root = root.as_ref();
        let canonical = std::fs::canonicalize(root)
            .map_err(|e| HarnessError::Effect(format!("file_read root {}: {e}", root.display())))?;
        if !canonical.is_dir() {
            return Err(HarnessError::Effect(format!(
                "file_read root {} is not a directory",
                root.display()
            )));
        }
        Ok(Self {
            name: DEFAULT_NAME.into(),
            root: canonical,
            max_bytes: DEFAULT_MAX_BYTES,
        })
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn err(&self, msg: impl std::fmt::Display) -> HarnessError {
        HarnessError::Effect(format!("{}: {msg}", self.name))
    }

    async fn read(&self, input: Value) -> Result<Value, HarnessError> {
        let rel = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| self.err("input needs a string `path`"))?;
        if !is_contained(Path::new(rel)) {
            return Err(self.err(format!("path `{rel}` escapes the root")));
        }

        let resolved = tokio::fs::canonicalize(self.root.join(rel))
            .await
            .map_err(|_| self.err(format!("path `{rel}` not found")))?;
        if !resolved.starts_with(&self.root) {
            return Err(self.err(format!("path `{rel}` escapes the root")));
        }

        let meta = tokio::fs::metadata(&resolved)
            .await
            .map_err(|e| self.err(format!("path `{rel}`: {e}")))?;
        if !meta.is_file() {
            return Err(self.err(format!("path `{rel}` is not a file")));
        }
        if meta.len() > self.max_bytes {
            return Err(self.err(format!(
                "path `{rel}` is {} bytes, over the {} byte limit",
                meta.len(),
                self.max_bytes
            )));
        }

        let file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|e| self.err(format!("path `{rel}`: {e}")))?;
        let mut buf = Vec::new();
        file.take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut buf)
            .await
            .map_err(|e| self.err(format!("path `{rel}`: {e}")))?;
        if buf.len() as u64 > self.max_bytes {
            return Err(self.err(format!(
                "path `{rel}` is over the {} byte limit",
                self.max_bytes
            )));
        }
        let bytes = buf.len();
        let contents = String::from_utf8(buf)
            .map_err(|_| self.err(format!("path `{rel}` is not UTF-8 text")))?;
        Ok(json!({"path": rel, "contents": contents, "bytes": bytes}))
    }
}

/// True when `path` is non-empty, relative, and has no `..`, root, or
/// prefix components. Pure: touches no filesystem.
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

impl Effect for FileRead {
    fn name(&self) -> &str {
        &self.name
    }

    fn capability(&self) -> Capability {
        Capability::FileRead
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(self.read(input))
    }
}
