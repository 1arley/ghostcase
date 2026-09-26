use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub target: String,
    #[serde(default = "default_max_runs")]
    pub max_runs: usize,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    pub oracle: OracleConfig,
    pub replacements: Vec<Replacement>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OracleConfig {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    pub path: String,
    #[serde(rename = "with")]
    pub with: Value,
}

fn default_max_runs() -> usize {
    500
}

fn default_timeout() -> u64 {
    15
}

pub fn load(path: &Path) -> Result<Config> {
    let source = fs::read_to_string(path).context("could not read configuration")?;
    // Avoid echoing TOML parser details: diagnostics can include user-supplied values.
    let config: Config =
        toml::from_str(&source).map_err(|_| anyhow::anyhow!("configuration is invalid"))?;
    anyhow::ensure!(!config.target.trim().is_empty(), "target must not be empty");
    anyhow::ensure!(config.max_runs >= 4, "max_runs must be at least 4");
    anyhow::ensure!(
        (1..=3600).contains(&config.timeout_seconds),
        "timeout_seconds must be between 1 and 3600"
    );
    anyhow::ensure!(
        !config.oracle.program.is_empty(),
        "oracle.program must not be empty"
    );
    anyhow::ensure!(
        !config.replacements.is_empty(),
        "at least one replacement is required"
    );
    anyhow::ensure!(
        config.oracle.args.iter().any(|arg| arg.contains("{input}")),
        "oracle.args must include the {{input}} placeholder"
    );

    let mut paths = std::collections::HashSet::new();
    for replacement in &config.replacements {
        anyhow::ensure!(
            replacement.path.starts_with('/') || replacement.path.is_empty(),
            "replacement paths must be JSON Pointers"
        );
        anyhow::ensure!(
            paths.insert(&replacement.path),
            "replacement JSON Pointers must be unique"
        );
        anyhow::ensure!(
            crate::json::is_scalar(&replacement.with),
            "replacement values must be JSON scalars"
        );
    }
    Ok(config)
}

pub fn resolve_program(config_path: &Path, program: &str) -> std::path::PathBuf {
    let path = Path::new(program);
    let explicitly_relative = program.starts_with("./") || program.starts_with(".\\");
    if path.is_absolute() || (!explicitly_relative && path.components().count() == 1) {
        path.to_owned()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}
