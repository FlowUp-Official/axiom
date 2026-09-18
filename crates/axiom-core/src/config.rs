use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::axm::codegen::ValidationOptions;
use crate::axm::ast::SafeParseMode;
use crate::errors::AxiomError;
use crate::paths::resolve_path;

pub const DEFAULT_CONFIG_FILE: &str = "axiom.json";

/// Canonical location of the JSON schema for the running CLI version. The tag
/// follows the `v<version>` convention used by the release workflow, so every
/// published release gets a dedicated, immutable schema URL.
pub const SCHEMA_VERSION_URL: &str = concat!(
    "https://raw.githubusercontent.com/FlowUp-Official/axiom/v",
    env!("CARGO_PKG_VERSION"),
    "/schemas/axiom.schema.json"
);

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
pub struct SourceConfig {
    /// Glob patterns for schema SQL files.
    pub schema: Vec<String>,
    /// Glob patterns for `.axm` files (models, types, and query declarations).
    /// Absent for projects that only generate from SQL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axm: Vec<String>,
}

/// A standalone validation API that may be generated for each model. `parse`
/// delegates to (`safeParse`)[#SafeParse], so listing `parse` needs no extra
/// configuration; model-level `@no_codegen`/`@target(...)`/`@safeParse(...)`
/// decorators still apply on top of this global selection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum ValidationApi {
    #[serde(rename = "safeParse")]
    SafeParse,
    #[serde(rename = "parse")]
    Parse,
}

/// How the `safeParse` API aggregates validation errors. Used as the global
/// default for models without an explicit `@safeParse("all"|"first")`
/// decorator.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SafeParseErrors {
    All,
    First,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SafeParseConfig {
    pub errors: SafeParseErrors,
}

/// `codegen.validation` selects which standalone validation APIs are generated
/// and how `safeParse` aggregates errors. The `safeParse` object is required
/// when (and only when) the `apis` list contains `safeParse`; `parse` in the
/// list needs no companion object because it has no error-collection mode of
/// its own. Enforced by the generated JSON schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCodegenConfig {
    /// The standalone validation APIs to generate, e.g. `["safeParse", "parse"]`.
    pub apis: Vec<ValidationApi>,
    /// Options for the `safeParse` API; required when `apis` includes
    /// `safeParse`.
    #[serde(rename = "safeParse")]
    pub safe_parse: Option<SafeParseConfig>,
}

impl JsonSchema for ValidationCodegenConfig {
    fn schema_name() -> String {
        "ValidationCodegenConfig".to_string()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::schema::Schema {
        serde_json::from_value(serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "apis": {
                    "type": "array",
                    "items": { "enum": ["safeParse", "parse"] },
                    "uniqueItems": true,
                    "minItems": 1
                },
                "safeParse": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "errors": { "enum": ["all", "first"] }
                    },
                    "required": ["errors"]
                }
            },
            "required": ["apis"],
            "allOf": [
                {
                    "if": {
                        "properties": {
                            "apis": { "contains": { "const": "safeParse" } }
                        },
                        "required": ["apis"]
                    },
                    "then": { "required": ["safeParse"] }
                }
            ]
        }))
        .expect("validation codegen schema is always valid JSON")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodegenConfig {
    pub validation: ValidationCodegenConfig,
}

impl CodegenConfig {
    /// Build the codegen [`ValidationOptions`] (which standalone validation
    /// APIs to emit and the default safeParse error-aggregation mode) from the
    /// `codegen.validation` section of the config.
    pub fn validation_options(&self) -> ValidationOptions {
        let v = &self.validation;
        ValidationOptions {
            emit_safe_parse: v.apis.contains(&ValidationApi::SafeParse),
            emit_parse: v.apis.contains(&ValidationApi::Parse),
            default_errors: v
                .safe_parse
                .as_ref()
                .map(|c| match c.errors {
                    SafeParseErrors::All => SafeParseMode::All,
                    SafeParseErrors::First => SafeParseMode::First,
                })
                .unwrap_or(SafeParseMode::All),
        }
    }
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
    /// `https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json`.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub project: ProjectConfig,
    pub cache: CacheConfig,
    pub source: SourceConfig,
    pub codegen: CodegenConfig,
    pub outputs: BTreeMap<String, OutputConfig>,
}

impl AxiomConfig {
    pub fn find_and_load(explicit_path: Option<&Path>) -> Result<(Self, PathBuf), AxiomError> {
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

    /// Build a fully-populated configuration with sensible defaults, suitable
    /// for bootstrapping a new project with `axiom init`.
    pub fn default_template() -> Self {
        let project_name = std::env::current_dir()
            .ok()
            .and_then(|dir| {
                dir.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "my-axiom-project".to_string());

        Self {
            schema: Some(SCHEMA_VERSION_URL.to_string()),
            project: ProjectConfig {
                name: project_name,
                dialect: "postgres".to_string(),
            },
            cache: CacheConfig::default(),
            source: SourceConfig {
                schema: vec!["./schema.sql".to_string()],
                axm: vec!["./models/**/*.axm".to_string()],
            },
            codegen: CodegenConfig {
                validation: ValidationCodegenConfig {
                    apis: vec![ValidationApi::SafeParse, ValidationApi::Parse],
                    safe_parse: Some(SafeParseConfig {
                        errors: SafeParseErrors::All,
                    }),
                },
            },
            outputs: BTreeMap::from([
                (
                    "api".to_string(),
                    OutputConfig::TypeScript(TypeScriptOutput {
                        path: PathBuf::from("./gen/api.ts"),
                    }),
                ),
                (
                    "core".to_string(),
                    OutputConfig::Rust(RustOutput {
                        path: PathBuf::from("./gen/api.rs"),
                    }),
                ),
            ]),
        }
    }

    /// Bootstrap a new `axiom.json` file at `path`, refusing to overwrite an
    /// existing file unless `force` is set.
    pub fn init_config(path: &Path, force: bool) -> Result<(), AxiomError> {
        if path.exists() && !force {
            return Err(AxiomError::ConfigAlreadyExists {
                path: path.display().to_string(),
            });
        }

        let contents = serde_json::to_string_pretty(&Self::default_template())
            .expect("AxiomConfig template is always serializable");

        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;

        Ok(())
    }
}

/// Resolve the configured glob patterns into ordered file paths, relative to
/// `base` (the directory containing the config file). Absolute patterns are
/// used as-is. Shared by `generate`, `check`, `format`, and `lint`.
pub fn resolve_glob_paths(patterns: &[String], base: &Path) -> Result<Vec<PathBuf>, AxiomError> {
    let mut paths = Vec::new();
    for pattern in patterns {
        let pattern_path = Path::new(pattern);
        let joined = if pattern_path.is_absolute() {
            pattern.to_string()
        } else {
            resolve_path(base, pattern_path)
                .to_string_lossy()
                .into_owned()
        };
        for path in glob::glob(&joined)? {
            paths.push(path?);
        }
    }
    Ok(paths)
}

/// Validate a raw `axiom.json` document against the generated JSON schema.
///
/// Returns the rendered validation errors on failure, and `()` on success.
fn validate_config_json(value: &serde_json::Value) -> Result<(), String> {
    let schema: serde_json::Value = serde_json::from_str(&AxiomConfig::generate_json_schema())
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
            "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json",
            "project": { "name": "fixture", "dialect": "postgres" },
            "cache": { "enabled": true, "path": ".axiom.cache" },
            "source": { "schema": ["schema.sql"], "axm": ["models/accounts.axm"] },
            "codegen": {
                "validation": {
                    "apis": ["safeParse", "parse"],
                    "safeParse": { "errors": "all" }
                }
            },
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
            .remove("source");

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(
            err.contains("source"),
            "expected the error to mention the missing key, got: {err}"
        );
    }

    #[test]
    fn safe_parse_in_apis_requires_safe_parse_object() {
        let mut value = valid_config();
        value["codegen"]["validation"]
            .as_object_mut()
            .expect("validation object")
            .remove("safeParse");

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(
            err.contains("required"),
            "expected a required-key error, got: {err}"
        );
    }

    #[test]
    fn safe_parse_object_not_required_when_apis_omits_safe_parse() {
        let mut value = valid_config();
        value["codegen"]["validation"]["apis"] =
            serde_json::json!(["parse"]);
        value["codegen"]["validation"]
            .as_object_mut()
            .expect("validation object")
            .remove("safeParse");

        assert_eq!(validate_config_json(&value), Ok(()));

        // `parse` in the list requires no `parse` companion object.
        value["codegen"]["validation"]["apis"] =
            serde_json::json!(["parse", "safeParse"]);
        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(
            err.contains("required"),
            "expected a required-key error once safeParse is listed, got: {err}"
        );
    }

    #[test]
    fn unknown_api_name_fails_json_schema_validation() {
        let mut value = valid_config();
        value["codegen"]["validation"]["apis"] = serde_json::json!(["nope"]);

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(!err.is_empty());
    }

    #[test]
    fn unknown_safe_parse_error_mode_fails_json_schema_validation() {
        let mut value = valid_config();
        value["codegen"]["validation"]["safeParse"]["errors"] =
            serde_json::json!("sometimes");

        let err = validate_config_json(&value).expect_err("expected validation failure");
        assert!(!err.is_empty());
    }

    fn fixture_path(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test_fixtures")
            .join(format!("init_{name}"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn default_template_is_valid_axiom_config() {
        let value = serde_json::to_value(AxiomConfig::default_template())
            .expect("template serializes to JSON");
        assert_eq!(validate_config_json(&value), Ok(()));

        let schema = value.get("$schema").and_then(|s| s.as_str());
        assert_eq!(schema, Some(SCHEMA_VERSION_URL));
        assert!(
            !schema.unwrap().contains("/main/"),
            "schema URL must pin the CLI version, not main: {}",
            schema.unwrap()
        );
    }

    #[test]
    fn init_config_creates_valid_file() {
        let path = fixture_path("create");
        AxiomConfig::init_config(&path, false).expect("init should succeed");

        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(validate_config_json(&value), Ok(()));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn init_config_refuses_overwrite_without_force() {
        let path = fixture_path("exists");
        AxiomConfig::init_config(&path, false).expect("first init should succeed");

        let err = AxiomConfig::init_config(&path, false)
            .expect_err("second init without --force should fail");
        assert!(
            matches!(err, AxiomError::ConfigAlreadyExists { .. }),
            "expected ConfigAlreadyExists, got: {err}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn init_config_overwrites_with_force() {
        let path = fixture_path("force");
        AxiomConfig::init_config(&path, false).expect("first init should succeed");

        std::fs::write(
            &path,
            r#"{ "project": { "name": "old", "dialect": "mysql" } }"#,
        )
        .unwrap();
        AxiomConfig::init_config(&path, true).expect("forced init should succeed");

        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(
            value["outputs"]["core"]["type"].is_string(),
            "template was written"
        );

        let _ = std::fs::remove_file(&path);
    }
}
