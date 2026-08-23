//! EKO-owned structured extraction surface service.
//!
//! The framework remains the sole owner of model execution and structured
//! response formatting through `ReactAgent::extract_json`. This service owns
//! product input validation, shared command parsing, and typed surface output.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::agent_handle::AgentHandle;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "StructuredExtractionRequest")]
pub struct StructuredExtractionRequest {
    pub input: String,
    pub schema: serde_json::Value,
    #[serde(default)]
    pub schema_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "StructuredExtractionOutcome")]
pub struct StructuredExtractionOutcome {
    pub success: bool,
    pub schema_name: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "StructuredExtractionValidation")]
pub struct StructuredExtractionValidation {
    pub valid: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, rename = "StructuredExtractionExample")]
pub struct StructuredExtractionExample {
    pub name: String,
    pub description: String,
    pub input: String,
    pub schema: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum StructuredExtractionError {
    #[error("structured extraction input is invalid: {0}")]
    InvalidInput(String),
    #[error("structured extraction schema is invalid: {0}")]
    InvalidSchema(String),
    #[error("structured extraction schema could not be read from {path}: {source}")]
    SchemaIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("structured extraction runtime is unavailable: {0}")]
    Runtime(String),
    #[error("structured extraction foreground admission failed: {0}")]
    Admission(String),
    #[error("structured extraction AgentPool admission failed: {0}")]
    AgentPool(String),
    #[error("structured extraction execution failed: {0}")]
    Execution(String),
    #[error("structured extraction output does not match the schema: {0}")]
    OutputSchema(String),
    #[error("structured extraction output serialization failed: {0}")]
    Serialization(String),
}

impl StructuredExtractionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "structured_extraction_input",
            Self::InvalidSchema(_) | Self::SchemaIo { .. } => "structured_extraction_schema",
            Self::Runtime(_) => "structured_extraction_runtime",
            Self::Admission(_) => "structured_extraction_admission",
            Self::AgentPool(_) => "structured_extraction_pool",
            Self::Execution(_) => "structured_extraction_execution",
            Self::OutputSchema(_) => "structured_extraction_output_schema",
            Self::Serialization(_) => "structured_extraction_serialization",
        }
    }

    pub fn is_validation(&self) -> bool {
        matches!(
            self,
            Self::InvalidInput(_) | Self::InvalidSchema(_) | Self::SchemaIo { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreparedStructuredExtractionCommand {
    Examples,
    Validate(serde_json::Value),
    Extract(StructuredExtractionRequest),
}

#[derive(Debug, Default)]
pub struct StructuredExtractionService;

impl StructuredExtractionService {
    pub fn validate_schema(&self, schema: &serde_json::Value) -> StructuredExtractionValidation {
        let mut errors = Vec::new();
        let Some(object) = schema.as_object() else {
            errors.push("Schema must be a JSON object".to_string());
            return StructuredExtractionValidation {
                valid: false,
                errors,
            };
        };
        if object.is_empty() {
            errors.push("Schema must not be empty".to_string());
        }
        if let Some(value) = object.get("properties")
            && !value.is_object()
        {
            errors.push("Schema properties must be a JSON object".to_string());
        }
        if let Some(value) = object.get("required") {
            match value.as_array() {
                Some(items) if items.iter().all(serde_json::Value::is_string) => {}
                _ => errors.push("Schema required must be an array of strings".to_string()),
            }
        }
        if errors.is_empty()
            && let Err(error) = jsonschema::validator_for(schema)
        {
            errors.push(format!("Schema is not a valid JSON Schema: {error}"));
        }
        StructuredExtractionValidation {
            valid: errors.is_empty(),
            errors,
        }
    }

    pub fn examples(&self) -> Vec<StructuredExtractionExample> {
        vec![StructuredExtractionExample {
            name: "person".to_string(),
            description: "Extract a person's name, age, and occupation".to_string(),
            input: "Zhang San, 28 years old, works as an engineer.".to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "age": {"type": "integer"},
                    "job": {"type": "string"}
                },
                "required": ["name", "age"],
                "additionalProperties": false
            }),
        }]
    }

    pub async fn extract(
        &self,
        agent: &AgentHandle,
        request: StructuredExtractionRequest,
    ) -> Result<StructuredExtractionOutcome, StructuredExtractionError> {
        let input = request.input.trim();
        if input.is_empty() {
            return Err(StructuredExtractionError::InvalidInput(
                "input must not be empty".to_string(),
            ));
        }
        let validation = self.validate_schema(&request.schema);
        if !validation.valid {
            return Err(StructuredExtractionError::InvalidSchema(
                validation.errors.join("; "),
            ));
        }
        let schema_name = normalized_schema_name(request.schema_name.as_deref())?;
        let validator = jsonschema::validator_for(&request.schema)
            .map_err(|error| StructuredExtractionError::InvalidSchema(error.to_string()))?;
        let format =
            echo_agent::llm::ResponseFormat::json_schema(schema_name.clone(), request.schema);
        let input = input.to_string();
        let data = agent
            .read_async(|agent| Box::pin(async move { agent.extract_json(&input, format).await }))
            .await
            .map_err(|error| StructuredExtractionError::Execution(error.to_string()))?;
        validator
            .validate(&data)
            .map_err(|error| StructuredExtractionError::OutputSchema(error.to_string()))?;
        Ok(StructuredExtractionOutcome {
            success: true,
            schema_name,
            data,
        })
    }

    pub fn parse_command(
        &self,
        command: &str,
    ) -> Result<PreparedStructuredExtractionCommand, StructuredExtractionError> {
        let (action, remainder) = take_argument(command).ok_or_else(command_usage)?;
        match action {
            "examples" | "example" => Ok(PreparedStructuredExtractionCommand::Examples),
            "validate" => {
                let source = required_argument(remainder, "validate <schema-json|@path>")?;
                Ok(PreparedStructuredExtractionCommand::Validate(
                    parse_schema_source(source)?,
                ))
            }
            "run" | "extract" => {
                let (schema_name, remainder) =
                    take_argument(remainder).ok_or_else(command_usage)?;
                let (schema_source, input) = remainder.split_once(" -- ").ok_or_else(|| {
                    StructuredExtractionError::InvalidInput(
                        "missing ` -- ` separator before input; usage: extract run <schema-name> <schema-json|@path> -- <input>"
                            .to_string(),
                    )
                })?;
                let input =
                    required_argument(input, "run <schema-name> <schema-json|@path> -- <input>")?;
                Ok(PreparedStructuredExtractionCommand::Extract(
                    StructuredExtractionRequest {
                        input: input.to_string(),
                        schema: parse_schema_source(schema_source)?,
                        schema_name: Some(schema_name.to_string()),
                    },
                ))
            }
            _ => Err(command_usage()),
        }
    }
}

fn normalized_schema_name(value: Option<&str>) -> Result<String, StructuredExtractionError> {
    let value = value.unwrap_or("extraction").trim();
    if value.is_empty() {
        return Ok("extraction".to_string());
    }
    if value.chars().count() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(StructuredExtractionError::InvalidSchema(
            "schema name must contain only ASCII letters, digits, '-' or '_' and be at most 64 characters"
                .to_string(),
        ));
    }
    Ok(value.to_string())
}

fn parse_schema_source(source: &str) -> Result<serde_json::Value, StructuredExtractionError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(StructuredExtractionError::InvalidSchema(
            "schema must not be empty".to_string(),
        ));
    }
    let encoded = match source.strip_prefix('@') {
        Some(path) if !path.trim().is_empty() => {
            let path = PathBuf::from(path.trim());
            fs::read_to_string(&path)
                .map_err(|source| StructuredExtractionError::SchemaIo { path, source })?
        }
        Some(_) => {
            return Err(StructuredExtractionError::InvalidSchema(
                "schema path after '@' must not be empty".to_string(),
            ));
        }
        None => source.to_string(),
    };
    serde_json::from_str(&encoded)
        .map_err(|error| StructuredExtractionError::InvalidSchema(error.to_string()))
}

fn take_argument(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((argument, remainder)) => Some((argument, remainder.trim_start())),
        None => Some((trimmed, "")),
    }
}

fn required_argument<'a>(
    value: &'a str,
    usage: &str,
) -> Result<&'a str, StructuredExtractionError> {
    let value = value.trim();
    if value.is_empty() {
        Err(StructuredExtractionError::InvalidInput(format!(
            "missing argument; usage: extract {usage}"
        )))
    } else {
        Ok(value)
    }
}

fn command_usage() -> StructuredExtractionError {
    StructuredExtractionError::InvalidInput(
        "usage: extract [examples|validate <schema-json|@path>|run <schema-name> <schema-json|@path> -- <input>]"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::agent::ReactAgentBuilder;
    use echo_agent::testing::MockLlmClient;
    use std::sync::Arc;

    #[test]
    fn command_parser_supports_shared_inline_and_file_contracts() -> Result<(), String> {
        let service = StructuredExtractionService;
        let parsed = service
            .parse_command(
                r#"run person {"type":"object","properties":{"name":{"type":"string"}}} -- Ada wrote the first program"#,
            )
            .map_err(|error| error.to_string())?;
        let PreparedStructuredExtractionCommand::Extract(request) = parsed else {
            return Err("run command did not prepare extraction".to_string());
        };
        assert_eq!(request.schema_name.as_deref(), Some("person"));
        assert_eq!(request.input, "Ada wrote the first program");

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = temp.path().join("schema.json");
        fs::write(&path, r#"{"type":"object"}"#).map_err(|error| error.to_string())?;
        let parsed = service
            .parse_command(&format!("validate @{}", path.display()))
            .map_err(|error| error.to_string())?;
        assert!(matches!(
            parsed,
            PreparedStructuredExtractionCommand::Validate(_)
        ));
        Ok(())
    }

    #[test]
    fn schema_validation_rejects_malformed_common_keywords() {
        let service = StructuredExtractionService;
        let validation = service.validate_schema(&serde_json::json!({
            "type": "object",
            "properties": [],
            "required": ["name", 7]
        }));
        assert!(!validation.valid);
        assert_eq!(validation.errors.len(), 2);
    }

    #[tokio::test]
    async fn extraction_returns_one_typed_outcome_from_framework_path() -> Result<(), String> {
        let agent = ReactAgentBuilder::new()
            .model("structured-extraction-test")
            .system_prompt("Extract structured data")
            .llm_client(Arc::new(
                MockLlmClient::new().with_response(r#"{"name":"Ada"}"#),
            ))
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let outcome = StructuredExtractionService
            .extract(
                &agent,
                StructuredExtractionRequest {
                    input: "Ada wrote the first program".to_string(),
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": {"name": {"type": "string"}},
                        "required": ["name"],
                        "additionalProperties": false
                    }),
                    schema_name: Some("person".to_string()),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(outcome.schema_name, "person");
        assert_eq!(outcome.data, serde_json::json!({"name": "Ada"}));
        Ok(())
    }

    #[tokio::test]
    async fn extraction_rejects_json_that_misses_the_requested_shape() -> Result<(), String> {
        let agent = ReactAgentBuilder::new()
            .model("structured-extraction-test")
            .system_prompt("Extract structured data")
            .llm_client(Arc::new(
                MockLlmClient::new().with_response(r#"{"age":"unknown"}"#),
            ))
            .build()
            .map(AgentHandle::new)
            .map_err(|error| error.to_string())?;
        let result = StructuredExtractionService
            .extract(
                &agent,
                StructuredExtractionRequest {
                    input: "Age is unknown".to_string(),
                    schema: serde_json::json!({
                        "type": "object",
                        "properties": {"age": {"type": "integer"}},
                        "required": ["age"],
                        "additionalProperties": false
                    }),
                    schema_name: Some("person".to_string()),
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(StructuredExtractionError::OutputSchema(_))
        ));
        Ok(())
    }
}
