//! Capability-scoped effects for the code-owned agent harness.
//!
//! Effects are named in Rust by a node and executed by Rust under a declared
//! capability. The model never receives a tool list and never names an
//! effect. The `Machine` refuses an effect whose capability was not granted.
//! See `SPEC.md`.
//!
//! - [`FileRead`]: reads UTF-8 files under a configured root
//!   ([`Capability::FileRead`](harness_core::Capability::FileRead)).
//! - [`HttpGet`]: GETs URLs on an explicit host allowlist
//!   ([`Capability::HttpGet`](harness_core::Capability::HttpGet)).
//! - [`Search`]: finds UTF-8 files under a configured root
//!   ([`Capability::FileRead`](harness_core::Capability::FileRead)).
//! - [`Shell`]: runs one fixed argv with an empty environment
//!   ([`Capability::DangerousShell`](harness_core::Capability::DangerousShell)).
//!   `run` stays off until [`Shell::grant`]. Never registered by default.
//! - [`Composio`]: runs one granted action. The key stays in the environment.

mod composio;
mod file_read;
mod http_get;
mod search;
mod shell;

pub use composio::{Composio, ExecuteCall};
pub use file_read::FileRead;
pub use http_get::HttpGet;
pub use search::Search;
pub use shell::Shell;
