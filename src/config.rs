use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::AxiomError;

pub const DEFAULT_CONFIG_FILE: &str = "axiom.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub dialect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputsConfig {
    pub schema: Vec<String>,
    pub queries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    pub on_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeScriptOutput {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustOutput {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxiomConfig {
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
        let config: AxiomConfig = serde_json::from_str(&contents)?;

        Ok((config, path))
    }

    pub fn target_types(&self) -> Vec<&str> {
        self.outputs
            .values()
            .map(|output| output.target_type())
            .collect()
    }
}
