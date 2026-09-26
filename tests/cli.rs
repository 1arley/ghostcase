use std::{fs, path::PathBuf, process::Command, thread, time::Duration};

use serde::Serialize;
use serde_json::{Value, json};
use tempfile::TempDir;

const INPUT: &str = include_str!("../examples/importer/input.json");
const ORACLE: &str = r#"
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as source:
        document = json.load(source)
    records = document["records"]
    ids = [record["id"] for record in records]
except (OSError, KeyError, TypeError, json.JSONDecodeError):
    result = {"outcome": "invalid_candidate"}
else:
    duplicate = len(ids) != len(set(ids))
    result = ({"outcome": "target_failure", "target": "import-duplicate-id"}
              if duplicate else {"outcome": "not_reproduced"})
print(json.dumps(result, separators=(",", ":")))
"#;

#[derive(Serialize)]
struct Config {
    target: String,
    max_runs: usize,
    timeout_seconds: u64,
    oracle: Oracle,
    replacements: Vec<Replacement>,
}

#[derive(Serialize)]
struct Oracle {
    program: String,
    args: Vec<String>,
}

#[derive(Serialize)]
struct Replacement {
    path: String,
    #[serde(rename = "with")]
    value: Value,
}

struct Invocation {
    _directory: TempDir,
    input_path: PathBuf,
    output_path: PathBuf,
    export_path: Option<PathBuf>,
    result: std::process::Output,
}

fn config() -> Config {
    Config {
        target: "import-duplicate-id".to_owned(),
        max_runs: 100,
        timeout_seconds: 5,
        oracle: Oracle {
            program: "python3".to_owned(),
            args: vec!["-c".to_owned(), ORACLE.to_owned(), "{input}".to_owned()],
        },
        replacements: ["/records/0/id", "/records/1/id"]
            .into_iter()
            .map(|path| Replacement {
                path: path.to_owned(),
                value: json!("EXAMPLE-USER"),
            })
            .collect(),
    }
}

fn invoke(input: &str, config: &Config) -> Invocation {
    invoke_with_options(input, config, None, false)
}

fn invoke_with_output(input: &str, config: &Config, existing_output: Option<&str>) -> Invocation {
    invoke_with_options(input, config, existing_output, false)
}

fn invoke_with_export(input: &str, config: &Config) -> Invocation {
    invoke_with_options(input, config, None, true)
}

fn invoke_with_options(
    input: &str,
    config: &Config,
    existing_output: Option<&str>,
    export: bool,
) -> Invocation {
    let directory = tempfile::tempdir().expect("temporary test directory");
    let input_path = directory.path().join("input.json");
    let config_path = directory.path().join("ghostcase.toml");
    let output_path = directory.path().join("minimized.json");
    let export_path = export.then(|| directory.path().join("bundle"));
    fs::write(&input_path, input).expect("write test input");
    fs::write(
        &config_path,
        toml::to_string(config).expect("serialize test config"),
    )
    .expect("write test config");
    if let Some(contents) = existing_output {
        fs::write(&output_path, contents).expect("write existing output");
    }

    run_invocation(directory, input_path, output_path, config_path, export_path)
}

fn run_invocation(
    directory: TempDir,
    input_path: PathBuf,
    output_path: PathBuf,
    config_path: PathBuf,
    export_path: Option<PathBuf>,
) -> Invocation {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ghostcase"));
    command.args([
        "minimize",
        "--input",
        input_path.to_str().expect("UTF-8 test path"),
        "--config",
        config_path.to_str().expect("UTF-8 test path"),
        "--output",
        output_path.to_str().expect("UTF-8 test path"),
    ]);
    if let Some(export_path) = &export_path {
        command.arg("--export-dir").arg(export_path);
    }
    let result = command.output().expect("run Ghostcase CLI");

    Invocation {
        _directory: directory,
        input_path,
        output_path,
        export_path,
        result,
    }
}

#[test]
fn minimizes_the_input_while_preserving_the_target_failure() {
    let invocation = invoke(INPUT, &config());
    assert!(
        invocation.result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&invocation.result.stdout),
        String::from_utf8_lossy(&invocation.result.stderr)
    );

    let report: Value = serde_json::from_slice(&invocation.result.stdout).expect("JSON report");
    let candidate: Value =
        serde_json::from_slice(&fs::read(&invocation.output_path).expect("sanitized candidate"))
            .expect("valid candidate JSON");

    assert_eq!(report["status"], "target_failure_preserved");
    assert_eq!(report["target"], "import-duplicate-id");
    assert_eq!(
        candidate,
        json!({
            "records": [
                {"id": "EXAMPLE-USER"},
                {"id": "EXAMPLE-USER"}
            ]
        })
    );
    assert_eq!(
        fs::read(&invocation.input_path).expect("original input"),
        INPUT.as_bytes()
    );
}

#[test]
fn exports_review_bundle_without_original_values() {
    let invocation = invoke_with_export(INPUT, &config());
    assert!(
        invocation.result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&invocation.result.stdout),
        String::from_utf8_lossy(&invocation.result.stderr)
    );

    let export_dir = invocation.export_path.as_ref().expect("export directory");
    let mut bundle = String::new();
    for name in ["candidate.json", "report.json", "recipe.json"] {
        let contents = fs::read(export_dir.join(name)).expect("bundle file");
        bundle.push_str(&String::from_utf8_lossy(&contents));
    }

    assert!(!bundle.contains("customer-user-secret"));
    let candidate: Value = serde_json::from_str(
        &fs::read_to_string(export_dir.join("candidate.json")).expect("bundle candidate"),
    )
    .expect("valid bundle candidate");
    assert_eq!(
        candidate,
        json!({
            "records": [
                {"id": "EXAMPLE-USER"},
                {"id": "EXAMPLE-USER"}
            ]
        })
    );
    let recipe: Value =
        serde_json::from_slice(&fs::read(export_dir.join("recipe.json")).expect("bundle recipe"))
            .expect("valid bundle recipe");
    assert_eq!(recipe["candidate"], "candidate.json");
    assert_eq!(recipe["target"], "import-duplicate-id");
}

#[test]
fn refuses_to_export_when_the_oracle_reports_a_different_target() {
    let mut configuration = config();
    configuration.target = "different-bug".to_owned();
    let invocation = invoke_with_export(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("original input did not consistently produce the configured target failure")
    );
    assert!(!invocation.output_path.exists());
    assert!(!invocation.export_path.expect("export directory").exists());
}

#[test]
fn rejects_duplicate_json_keys_without_exporting_a_candidate() {
    let invocation = invoke(r#"{"records":[],"records":[]}"#, &config());

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("input is invalid or unsupported JSON")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn refuses_bundle_when_recipe_contains_a_protected_value() {
    let mut configuration = config();
    configuration
        .oracle
        .args
        .push("customer-user-secret".to_owned());
    let invocation = invoke_with_export(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("the review bundle would contain a protected original value")
    );
    assert!(!invocation.output_path.exists());
    assert!(!invocation.export_path.expect("export directory").exists());
}

#[test]
fn refuses_to_export_when_a_protected_value_remains_in_another_string() {
    let input = INPUT.replace(
        "customer-export",
        "archive-customer-user-secret-customer-export",
    );
    let invocation = invoke(&input, &config());

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("a protected original value remains in the candidate")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn stops_reduction_at_the_configured_run_budget() {
    let mut configuration = config();
    configuration.max_runs = 4;
    let invocation = invoke(INPUT, &configuration);

    assert!(
        invocation.result.status.success(),
        "{}",
        String::from_utf8_lossy(&invocation.result.stderr)
    );
    let report: Value = serde_json::from_slice(&invocation.result.stdout).expect("JSON report");
    assert_eq!(report["executions"], 4);
    assert_eq!(report["budget_exhausted"], true);
}

#[test]
fn times_out_an_oracle_without_creating_a_candidate() {
    let mut configuration = config();
    configuration.timeout_seconds = 1;
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "import time; time.sleep(30)".to_owned(),
        "{input}".to_owned(),
    ];
    let invocation = invoke(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("oracle timed out or exceeded its output limit")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn times_out_oracle_descendants() {
    let mut configuration = config();
    configuration.timeout_seconds = 1;
    let marker_directory = tempfile::tempdir().expect("create marker directory");
    let marker_path = marker_directory.path().join("orphan");
    let parent_code = "import subprocess,sys,time; subprocess.Popen([sys.executable, '-c', \"import pathlib,sys,time; time.sleep(2); pathlib.Path(sys.argv[1]).write_text('orphan')\", sys.argv[1]]); time.sleep(30)";
    configuration.oracle.args = vec![
        "-c".to_owned(),
        parent_code.to_owned(),
        marker_path.to_str().expect("UTF-8 marker path").to_owned(),
        "{input}".to_owned(),
    ];

    let invocation = invoke(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("oracle timed out or exceeded its output limit")
    );
    thread::sleep(Duration::from_millis(2200));
    assert!(!marker_path.exists());
    assert!(!invocation.output_path.exists());
}

#[test]
fn rejects_oracle_output_over_the_limit() {
    let mut configuration = config();
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "import sys,time; sys.stdout.write('x' * 20000); sys.stdout.flush(); time.sleep(30)"
            .to_owned(),
        "{input}".to_owned(),
    ];
    let invocation = invoke(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("oracle exceeded its output limit")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn rejects_malformed_oracle_protocol() {
    let mut configuration = config();
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "print('not-json')".to_owned(),
        "{input}".to_owned(),
    ];
    let invocation = invoke(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("oracle returned an invalid protocol response")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn refuses_to_export_when_replacements_remove_the_target_failure() {
    let mut configuration = config();
    configuration.replacements[1].value = json!("DIFFERENT-USER");
    let invocation = invoke(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("candidate did not reproduce the configured target failure")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn never_overwrites_an_existing_output_file() {
    let invocation = invoke_with_output(INPUT, &config(), Some("keep this file"));

    assert!(!invocation.result.status.success());
    assert_eq!(
        fs::read(&invocation.output_path).expect("existing output"),
        b"keep this file"
    );
}

#[test]
fn preserves_exact_large_json_numbers_that_are_needed_to_reproduce_the_bug() {
    const LARGE_NUMBER: &str = "1234567890123456789012345678901234567890";
    let input = format!(
        r#"{{"sentinel":{LARGE_NUMBER},"records":[{{"id":"private-user"}},{{"id":"private-user"}},{{"id":"other-user"}}]}}"#
    );
    let mut configuration = config();
    configuration.oracle.program = "python3".to_owned();
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "import json,sys; d=json.load(open(sys.argv[1])); print('{\"outcome\":\"target_failure\",\"target\":\"import-duplicate-id\"}' if d.get('sentinel') else '{\"outcome\":\"not_reproduced\"}')".to_owned(),
        "{input}".to_owned(),
    ];
    let invocation = invoke(&input, &configuration);
    assert!(invocation.result.status.success());
    let candidate = fs::read_to_string(&invocation.output_path).expect("candidate JSON");
    assert!(candidate.contains(LARGE_NUMBER));
    assert!(candidate.starts_with(&format!("{{\n  \"sentinel\": {LARGE_NUMBER}")));
}
