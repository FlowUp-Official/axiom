use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::errors::AxiomError;

pub const DEFAULT_CONFIG_FILE: &str = "axiom.json";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectConfig {
    pub name: String,
    pub dialect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CacheConfig {
    pub enabled: bool,
    pub path: PathBuf,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from(".axiom.cache"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InputsConfig {
    pub schema: Vec<String>,
    pub queries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ValidationConfig {
    pub on_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OutputConfig {
    TypeScript(TypeScriptOutput),
    Rust(RustOutput),
}

impl OutputConfig {
    pub fn target_type(&self) -> &'static str {
        match self {
            OutputConfig::TypeScript(_) => "typescript",
            OutputConfig::Rust(_) => "rust",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TypeScriptOutput {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RustOutput {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AxiomConfig {
    /// URL pointing to the Axiom JSON schema version, e.g.
    /// `https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.5.0/schemas/axiom.schema.json`.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub project: ProjectConfig,
    pub cache: CacheConfig,
    pub inputs: InputsConfig,
    pub validation: ValidationConfig,
    pub outputs: BTreeMap<String, OutputConfig>,
}

impl AxiomConfig {
    pub fn find_and_load(
        explicit_path: Option<&Path>,
    ) -> Result<(Self, PathBuf), AxiomError> {
        let path = match explicit_path {
            Some(p) => p.to_path_buf(),
            None => {
                let default_path = PathBuf::from(DEFAULT_CONFIG_FILE);
                if !default_path.exists() {
                    return Err(AxiomError::MissingConfig);
                }
                default_path
            }
        };

        if !path.exists() {
            return Err(AxiomError::ConfigNotFound(path));
        }

        let contents = std::fs::read_to_string(&path)?;
        let value: serde_json::Value = serde_json::from_str(&contents)?;

        validate_config_json(&value).map_err(|errors| AxiomError::ConfigValidationFailed {
            path: path.clone(),
            errors,
        })?;

        let config: AxiomConfig = serde_json::from_value(value)?;

        Ok((config, path))
    }

    pub fn target_types(&self) -> Vec<&str> {
        self.outputs
            .values()
            .map(|output| output.target_type())
            .collect()
    }

    /// Serialize the JSON schema for `axiom.json` files to a pretty-printed
    /// string, used by the `axiom schema` command and release packaging.
    pub fn generate_json_schema() -> String {
        let schema = schemars::schema_for!(AxiomConfig);
        serde_json::to_string_pretty(&schema).expect("AxiomConfig schema is always serializable")
    }
}

/// Validate a raw `axiom.json` document against the generated JSON schema.
///
/// Returns the rendered validation errors on failure, and `()` on success.
fn validate_config_json(value: &serde_json::Value) -> Result<(), String> {
    let schema: serde_json::Value =
        serde_json::from_str(&AxiomConfig::generate_json_schema())
            .expect("generated schema is always valid JSON");
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| format!("failed to build schema validator: {error}"))?;

    match validator.validate(value) {
        Ok(()) => Ok(()),
        Err(_) => Err(validator
            .iter_errors(value)
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> serde_json::Value {
        serde_json::json!({
            "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.5.0/schemas/axiom.schema.json",
            "project": { "name": "fixture", "dialect": "postgres" },
            "cache": { "enabled": true, "path": ".axiom.cache" },
            "inputs": { "schema": ["schema.sql"], "queries": ["queries/accounts.sql"] },
            "validation": { "on_error": "fail" },
            "outputs": {
                "api": { "type": "typescript", "path": "gen/api.ts" }
            }
        })
    }

    #[test]
    fn valid_config_passes_json_schema_validation() {
        let value = valid_config();
        assert_eq!(validate_config_json(&value), Ok(()));
    }

    #[test]
    fn schema_without_dollar_schema_key_still_valid() {
        let mut value = valid_config();
        value
            .as_object_mut()
            .expect("config is an object")
            .remove("$schema");
        assert_eq!(validate_config_json(&value), Ok(()));
    }

    #[test]
    fn invalid_key_type_fails_json_schema_validation() {
        let mut value = valid_config();
        value["cache"]["enabled"] = serde_json::Value::String("yes".into());

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(
            err.contains("boolean"),
            "expected a type error mentioning booleans, got: {err}"
        );
    }

    #[test]
    fn unknown_output_type_fails_json_schema_validation() {
        let mut value = valid_config();
        value["outputs"]["api"]["type"] = serde_json::Value::String("cobol".into());

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(!err.is_empty());
    }

    #[test]
    fn missing_required_field_fails_json_schema_validation() {
        let mut value = valid_config();
        value
            .as_object_mut()
            .expect("config is an object")
            .remove("inputs");

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(
            err.contains("inputs"),
            "expected the error to mention the missing key, got: {err}"
        );
    }
}
