//! Core of the code-owned agent harness.
//!
//! Hosts the `Machine`, its explicit node graph, the `DecisionModel`,
//! `GenerativeModel`, and `Effect` traits, and the append-only event log.
//! Code owns control flow: the next step is always a Rust match or
//! state-machine transition, never model output. See `SPEC.md`.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {}
}
