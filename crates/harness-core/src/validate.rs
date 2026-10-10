use crate::error::HarnessError;
use crate::generate::{GenOutput, JsonSchema, OutputSpec};

/// Checks a generative output against its spec before it reaches state or a
/// transition. A bad schema is a workflow error; a bad output is
/// `SchemaInvalid`. Providers call it before returning; the `Machine` checks
/// again.
pub fn validate_output(spec: &OutputSpec, output: &GenOutput) -> Result<(), HarnessError> {
    match (spec, output) {
        (OutputSpec::Schema(JsonSchema(schema)), GenOutput::Json(value)) => {
            let validator = jsonschema::validator_for(schema)
                .map_err(|e| HarnessError::Workflow(format!("invalid JSON schema: {e}")))?;
            validator
                .validate(value)
                .map_err(|e| HarnessError::SchemaInvalid(e.to_string()))
        }
        (OutputSpec::Schema(_), GenOutput::Text(_)) => Err(HarnessError::SchemaInvalid(
            "expected JSON for a schema-bound node, got free text".into(),
        )),
        (OutputSpec::FreeText, GenOutput::Text(_)) => Ok(()),
        (OutputSpec::FreeText, GenOutput::Json(_)) => Err(HarnessError::SchemaInvalid(
            "expected free text, got JSON".into(),
        )),
    }
}
