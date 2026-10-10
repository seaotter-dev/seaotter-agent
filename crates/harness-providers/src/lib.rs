//! Model providers for the code-owned agent harness.
//!
//! System One decision clients (TypeSafe Jev and compatible `/v1/systemone`
//! endpoints) implement `DecisionModel`. OpenAI-compatible and Anthropic
//! clients implement `GenerativeModel` as single-shot, schema-bounded calls
//! that never choose tools or the next step. See `SPEC.md`.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {}
}
