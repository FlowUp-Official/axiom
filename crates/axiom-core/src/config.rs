use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
pub struct InputsConfig {
    pub schema: Vec<String>,
    pub queries: Vec<String>,
    /// Glob patterns for `.axm` domain-model files. Absent for projects that
    /// only generate from SQL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
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
    /// `https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json`.
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

    /// Build a fully-populated configuration with sensible defaults, suitable
    /// for bootstrapping a new project with `axiom init`.
    pub fn default_template() -> Self {
        let project_name = std::env::current_dir()
            .ok()
            .and_then(|dir| dir.file_name().map(|name| name.to_string_lossy().into_owned()))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "my-axiom-project".to_string());

        Self {
            schema: Some(SCHEMA_VERSION_URL.to_string()),
            project: ProjectConfig {
                name: project_name,
                dialect: "postgres".to_string(),
            },
            cache: CacheConfig::default(),
            inputs: InputsConfig {
                schema: vec!["./schema.sql".to_string()],
                queries: vec!["./queries/**/*.sql".to_string()],
                models: vec!["./models/**/*.axm".to_string()],
            },
            validation: ValidationConfig {
                on_error: "fail".to_string(),
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
pub fn resolve_glob_paths(
    patterns: &[String],
    base: &Path,
) -> Result<Vec<PathBuf>, AxiomError> {
    let mut paths = Vec::new();
    for pattern in patterns {
        let pattern_path = Path::new(pattern);
        let joined = if pattern_path.is_absolute() {
            pattern.to_string()
        } else {
            resolve_path(base, pattern_path).to_string_lossy().into_owned()
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
            "$schema": "https://raw.githubusercontent.com/FlowUp-Official/axiom/v0.6.0/schemas/axiom.schema.json",
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

        std::fs::write(&path, r#"{ "project": { "name": "old", "dialect": "mysql" } }"#).unwrap();
        AxiomConfig::init_config(&path, true).expect("forced init should succeed");

        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(value["outputs"]["core"]["type"].is_string(), "template was written");

        let _ = std::fs::remove_file(&path);
    }
}
