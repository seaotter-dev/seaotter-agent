use std::path::{Path, PathBuf};

use harness_core::{BoxFuture, Capability, Effect, HarnessError};
use serde_json::{Value, json};
use tokio::fs;

pub const DEFAULT_NAME: &str = "search";
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

/// Finds UTF-8 files under a fixed root whose contents contain a query.
/// A symlink that resolves outside the root is not read.
#[derive(Debug, Clone)]
pub struct Search {
    name: String,
    root: PathBuf,
    max_bytes: u64,
}

impl Search {
    /// `root` must be an existing directory. It is canonicalized once here.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, HarnessError> {
        let root = root.as_ref();
        let canonical = std::fs::canonicalize(root)
            .map_err(|e| HarnessError::Effect(format!("search root {}: {e}", root.display())))?;
        if !canonical.is_dir() {
            return Err(HarnessError::Effect(format!(
                "search root {} is not a directory",
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

    fn err(&self, msg: impl std::fmt::Display) -> HarnessError {
        HarnessError::Effect(format!("{}: {msg}", self.name))
    }

    async fn search(&self, input: Value) -> Result<Value, HarnessError> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| self.err("input needs a string `query`"))?;
        let mut paths = Vec::new();
        self.walk(&self.root, query, &mut paths).await?;
        paths.sort();
        Ok(json!({"paths": paths}))
    }

    async fn walk(
        &self,
        dir: &Path,
        query: &str,
        paths: &mut Vec<String>,
    ) -> Result<(), HarnessError> {
        let mut entries = fs::read_dir(dir)
            .await
            .map_err(|e| self.err(format!("reading {}: {e}", dir.display())))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| self.err(format!("reading {}: {e}", dir.display())))?
        {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)
                .await
                .map_err(|e| self.err(format!("path {}: {e}", path.display())))?;
            if meta.file_type().is_symlink() {
                self.consider_link(&path, query, paths).await?;
                continue;
            }
            if meta.is_dir() {
                Box::pin(self.walk(&path, query, paths)).await?;
                continue;
            }
            if meta.is_file() {
                self.consider_file(&path, &path, query, paths).await?;
            }
        }
        Ok(())
    }

    async fn consider_link(
        &self,
        path: &Path,
        query: &str,
        paths: &mut Vec<String>,
    ) -> Result<(), HarnessError> {
        let Ok(resolved) = fs::canonicalize(path).await else {
            return Ok(());
        };
        if !resolved.starts_with(&self.root) || !resolved.is_file() {
            return Ok(());
        }
        self.consider_file(path, &resolved, query, paths).await
    }

    async fn consider_file(
        &self,
        reported: &Path,
        source: &Path,
        query: &str,
        paths: &mut Vec<String>,
    ) -> Result<(), HarnessError> {
        let meta = fs::metadata(source)
            .await
            .map_err(|e| self.err(format!("path {}: {e}", source.display())))?;
        if !meta.is_file() || meta.len() > self.max_bytes {
            return Ok(());
        }
        let buf = fs::read(source)
            .await
            .map_err(|e| self.err(format!("path {}: {e}", source.display())))?;
        if buf.len() as u64 > self.max_bytes {
            return Ok(());
        }
        let Ok(contents) = String::from_utf8(buf) else {
            return Ok(());
        };
        if !contents.contains(query) {
            return Ok(());
        }
        let rel = reported
            .strip_prefix(&self.root)
            .map_err(|_| self.err(format!("path {} escapes the root", reported.display())))?;
        paths.push(rel.to_string_lossy().into_owned());
        Ok(())
    }
}

impl Effect for Search {
    fn name(&self) -> &str {
        &self.name
    }

    fn capability(&self) -> Capability {
        Capability::FileRead
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(self.search(input))
    }
}
