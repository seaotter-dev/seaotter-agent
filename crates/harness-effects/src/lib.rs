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

mod file_read;
mod http_get;

pub use file_read::FileRead;
pub use http_get::HttpGet;
