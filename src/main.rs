use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use tempfile::NamedTempFile;

mod config;
mod json;
mod oracle;
mod reduce;

#[derive(Parser)]
#[command(
    name = "ghostcase",
    version,
    about = "Create smaller bug reproductions from JSON data"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Replace protected JSON fields and reduce the input while preserving a target failure.
    Minimize {
        /// Original JSON input. It is never modified.
        #[arg(long)]
        input: PathBuf,
        /// TOML run configuration.
        #[arg(long)]
        config: PathBuf,
        /// New path for the sanitized candidate. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
        /// Optional new directory for the candidate, report and recipe. Existing paths are never overwritten.
        #[arg(long)]
        export_dir: Option<PathBuf>,
    },
}

#[derive(Serialize)]
struct Report<'a> {
    status: &'static str,
    target: &'a str,
    executions: usize,
    original_bytes: usize,
    output_bytes: usize,
    removed_bytes: usize,
    budget_exhausted: bool,
}

#[derive(Serialize)]
struct Recipe<'a> {
    target: &'a str,
    candidate: &'static str,
    oracle: OracleRecipe<'a>,
    expected_outcome: &'static str,
}

#[derive(Serialize)]
struct OracleRecipe<'a> {
    program: &'a str,
    args: &'a [String],
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ghostcase: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Minimize {
            input,
            config: config_path,
            output,
            export_dir,
        } => minimize(&input, &config_path, &output, export_dir.as_deref()),
    }
}

fn minimize(
    input_path: &Path,
    config_path: &Path,
    output_path: &Path,
    export_dir: Option<&Path>,
) -> Result<()> {
    anyhow::ensure!(input_path.is_file(), "input must be a regular file");
    anyhow::ensure!(
        input_path != output_path,
        "output must not be the input file"
    );
    anyhow::ensure!(
        !output_path.exists(),
        "output already exists; choose a new path"
    );
    if let Some(export_dir) = export_dir {
        anyhow::ensure!(
            !export_dir.exists(),
            "export directory already exists; choose a new path"
        );
        anyhow::ensure!(
            parent_dir(export_dir).is_dir(),
            "export directory parent does not exist"
        );
    }

    let config = config::load(config_path)?;
    let original_bytes = fs::read(input_path).context("could not read input JSON")?;
    let mut document =
        json::parse(&original_bytes).context("input is invalid or unsupported JSON")?;
    let original_size = original_bytes.len();

    let mut protected = Vec::with_capacity(config.replacements.len());
    for replacement in &config.replacements {
        let original = document
            .pointer(&replacement.path)
            .with_context(|| {
                format!(
                    "protected JSON Pointer does not exist: {}",
                    replacement.path
                )
            })?
            .clone();
        anyhow::ensure!(
            json::is_scalar(&original) && json::is_scalar(&replacement.with),
            "protected fields and replacement values must be JSON scalars"
        );
        anyhow::ensure!(
            original != replacement.with,
            "a replacement must differ from its original value"
        );
        protected.push(original);
    }

    let mut oracle = oracle::new(&config.oracle, config_path)?;
    let mut executions = 0;
    for _ in 0..2 {
        let outcome = oracle::run(
            &mut oracle,
            &document,
            &config.target,
            config.timeout_seconds,
        )?;
        executions += 1;
        anyhow::ensure!(
            matches!(outcome, oracle::Outcome::TargetFailure),
            "the original input did not consistently produce the configured target failure"
        );
    }

    for replacement in &config.replacements {
        let value = document
            .pointer_mut(&replacement.path)
            .context("protected JSON Pointer became invalid")?;
        *value = replacement.with.clone();
    }
    anyhow::ensure!(
        !json::contains_any(&document, &protected),
        "a protected original value remains in the candidate; review the declared JSON Pointers"
    );

    ensure_target_failure(
        &mut oracle,
        &document,
        &config.target,
        config.timeout_seconds,
        &mut executions,
    )?;

    let reduction = reduce::minimize(
        &mut document,
        &protected,
        config.max_runs.saturating_sub(executions + 1),
        |candidate| {
            let outcome = oracle::run(
                &mut oracle,
                candidate,
                &config.target,
                config.timeout_seconds,
            )?;
            executions += 1;
            Ok(outcome == oracle::Outcome::TargetFailure)
        },
    )?;

    ensure_target_failure(
        &mut oracle,
        &document,
        &config.target,
        config.timeout_seconds,
        &mut executions,
    )?;
    anyhow::ensure!(
        !json::contains_any(&document, &protected),
        "a protected original value remains in the final candidate"
    );

    let output_bytes =
        serde_json::to_vec_pretty(&document).context("could not serialize candidate")?;
    let report = Report {
        status: "target_failure_preserved",
        target: &config.target,
        executions,
        original_bytes: original_size,
        output_bytes: output_bytes.len(),
        removed_bytes: original_size.saturating_sub(output_bytes.len()),
        budget_exhausted: reduction.budget_exhausted,
    };

    write_candidate(output_path, &output_bytes)?;
    if let Some(export_dir) = export_dir
        && let Err(error) = write_bundle(export_dir, &output_bytes, &report, &config, &protected)
    {
        let _ = fs::remove_file(output_path);
        return Err(error);
    }

    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn write_candidate(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = parent_dir(path);
    anyhow::ensure!(parent.is_dir(), "output directory does not exist");
    anyhow::ensure!(!path.exists(), "output already exists; choose a new path");

    let mut candidate =
        NamedTempFile::new_in(parent).context("could not create temporary output")?;
    candidate
        .write_all(bytes)
        .context("could not write temporary output")?;
    candidate
        .as_file()
        .sync_all()
        .context("could not sync candidate")?;
    candidate
        .persist_noclobber(path)
        .map_err(|_| anyhow::anyhow!("could not create output; choose a new path"))?;
    Ok(())
}

fn write_bundle(
    export_dir: &Path,
    candidate_bytes: &[u8],
    report: &Report<'_>,
    config: &config::Config,
    protected: &[serde_json::Value],
) -> Result<()> {
    anyhow::ensure!(
        !export_dir.exists(),
        "export directory already exists; choose a new path"
    );
    let parent = parent_dir(export_dir);
    anyhow::ensure!(parent.is_dir(), "export directory parent does not exist");
    fs::create_dir(export_dir).context("could not create export directory")?;

    let result = write_bundle_files(export_dir, candidate_bytes, report, config, protected);
    if result.is_err() {
        let _ = fs::remove_dir_all(export_dir);
    }
    result
}

fn write_bundle_files(
    export_dir: &Path,
    candidate_bytes: &[u8],
    report: &Report<'_>,
    config: &config::Config,
    protected: &[serde_json::Value],
) -> Result<()> {
    let recipe = Recipe {
        target: &config.target,
        candidate: "candidate.json",
        oracle: OracleRecipe {
            program: &config.oracle.program,
            args: &config.oracle.args,
        },
        expected_outcome: "target_failure",
    };
    let recipe_bytes = serde_json::to_vec_pretty(&recipe).context("could not serialize recipe")?;
    let report_bytes = serde_json::to_vec_pretty(report).context("could not serialize report")?;
    let recipe_value: serde_json::Value =
        serde_json::from_slice(&recipe_bytes).context("could not validate recipe")?;
    let report_value: serde_json::Value =
        serde_json::from_slice(&report_bytes).context("could not validate report")?;
    let protected_text: Vec<_> = protected
        .iter()
        .filter(|value| value.is_string())
        .cloned()
        .collect();
    anyhow::ensure!(
        !json::contains_any(&recipe_value, &protected_text)
            && !json::contains_any(&report_value, &protected_text),
        "the review bundle would contain a protected original value"
    );

    write_bundle_file(export_dir, "candidate.json", candidate_bytes)?;
    write_bundle_file(export_dir, "report.json", &report_bytes)?;
    write_bundle_file(export_dir, "recipe.json", &recipe_bytes)?;
    Ok(())
}

fn write_bundle_file(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let mut file = NamedTempFile::new_in(directory).context("could not create bundle file")?;
    file.write_all(bytes)
        .context("could not write bundle file")?;
    file.as_file()
        .sync_all()
        .context("could not sync bundle file")?;
    file.persist_noclobber(directory.join(name))
        .map_err(|_| anyhow::anyhow!("could not create bundle file"))?;
    Ok(())
}

fn ensure_target_failure(
    oracle: &mut oracle::Oracle,
    document: &serde_json::Value,
    target: &str,
    timeout_seconds: u64,
    executions: &mut usize,
) -> Result<()> {
    let outcome = oracle::run(oracle, document, target, timeout_seconds)?;
    *executions += 1;
    anyhow::ensure!(
        outcome == oracle::Outcome::TargetFailure,
        "candidate did not reproduce the configured target failure"
    );
    Ok(())
}
