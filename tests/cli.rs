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
    /// Omitted by default so the common configuration stays minimal; `contains`
    /// opts into the strict substring rule.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    match_mode: Option<&'static str>,
}

impl Replacement {
    fn new(path: &str, value: Value) -> Self {
        Self {
            path: path.to_owned(),
            value,
            match_mode: None,
        }
    }

    fn matching_anywhere(mut self) -> Self {
        self.match_mode = Some("contains");
        self
    }
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
            .map(|path| Replacement::new(path, json!("EXAMPLE-USER")))
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

/// Runs a fixture exactly as shipped: the real configuration and the real
/// adapter from `examples/`, with artifacts written to a temporary directory.
fn invoke_fixture(name: &str) -> Invocation {
    let directory = tempfile::tempdir().expect("temporary test directory");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name);
    let output_path = directory.path().join("minimized.json");
    let export_path = directory.path().join("bundle");
    run_invocation(
        directory,
        fixture.join("input.json"),
        output_path,
        fixture.join("ghostcase.toml"),
        Some(export_path),
    )
}

fn stdout_of(invocation: &Invocation) -> String {
    String::from_utf8_lossy(&invocation.result.stdout).into_owned()
}

fn stderr_of(invocation: &Invocation) -> String {
    String::from_utf8_lossy(&invocation.result.stderr).into_owned()
}

fn assert_success(invocation: &Invocation) {
    assert!(
        invocation.result.status.success(),
        "stdout: {}\nstderr: {}",
        stdout_of(invocation),
        stderr_of(invocation)
    );
}

fn read_bundle(invocation: &Invocation) -> String {
    let export_dir = invocation.export_path.as_ref().expect("export directory");
    let mut bundle = String::new();
    for name in ["candidate.json", "report.json", "recipe.json"] {
        let contents = fs::read(export_dir.join(name)).unwrap_or_else(|error| {
            panic!("read {} from the bundle: {error}", name);
        });
        bundle.push_str(&String::from_utf8_lossy(&contents));
    }
    bundle
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
    let mut configuration = config();
    // The strict substring rule is opt-in, so declare it.
    for replacement in &mut configuration.replacements {
        *replacement =
            std::mem::replace(replacement, Replacement::new("", json!("x"))).matching_anywhere();
    }
    let invocation = invoke(&input, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr)
            .contains("a protected original value remains in the candidate")
    );
    assert!(!invocation.output_path.exists());
}

#[test]
fn exact_mode_does_not_reject_an_unrelated_prefix_overlapping_identifier() {
    // Protecting `user-1` must not be read as leaking into the unrelated `user-10`.
    // Substring matching made Ghostcase unusable on sequential identifiers.
    let input = r#"{"records":[{"id":"user-1"},{"id":"user-1"},{"id":"user-10"}]}"#;
    let mut configuration = config();
    configuration.replacements = ["/records/0/id", "/records/1/id"]
        .into_iter()
        .map(|path| Replacement::new(path, json!("EXAMPLE-USER")))
        .collect();

    let invocation = invoke(input, &configuration);

    assert!(
        invocation.result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&invocation.result.stdout),
        String::from_utf8_lossy(&invocation.result.stderr)
    );
    let candidate: Value =
        serde_json::from_slice(&fs::read(&invocation.output_path).expect("candidate"))
            .expect("valid candidate");
    let ids: Vec<&str> = candidate["records"]
        .as_array()
        .expect("records")
        .iter()
        .map(|record| record["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["EXAMPLE-USER", "EXAMPLE-USER"]);
    // The unrelated identifier survives; only the protected one was replaced.
    assert!(!candidate.to_string().contains("user-1"));
}

#[test]
fn keeps_reducing_when_the_adapter_raises_on_a_reduced_candidate() {
    // Reduction removes the fields the adapter reads, so an adapter that raises
    // instead of answering used to abort the whole run. Such a candidate is now
    // skipped, never accepted, and the reduction still succeeds.
    let mut configuration = config();
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "import json,sys\nd=json.load(open(sys.argv[1]))\ntry:\n ids=[r['id'] for r in d['records']]\nexcept (KeyError,TypeError):\n raise SystemExit(3)\nprint(json.dumps({'outcome':'target_failure','target':'import-duplicate-id'} if len(ids)!=len(set(ids)) else {'outcome':'not_reproduced'}))"
            .to_owned(),
        "{input}".to_owned(),
    ];

    let invocation = invoke(INPUT, &configuration);

    assert!(
        invocation.result.status.success(),
        "the run must survive a raising adapter, stderr: {}",
        String::from_utf8_lossy(&invocation.result.stderr)
    );
    let report: Value = serde_json::from_slice(&invocation.result.stdout).expect("JSON report");
    assert_eq!(report["status"], "target_failure_preserved");
    assert!(
        report["untestable_candidates"].as_u64().expect("count") > 0,
        "the fixture must produce untestable candidates"
    );
    // The final candidate is still a valid, minimized reproduction.
    let candidate: Value =
        serde_json::from_slice(&fs::read(&invocation.output_path).expect("candidate"))
            .expect("valid candidate");
    assert_eq!(
        candidate,
        json!({
            "records": [
                {"id": "EXAMPLE-USER"},
                {"id": "EXAMPLE-USER"}
            ]
        })
    );
    // A raising adapter is worth telling the user about.
    let stderr = String::from_utf8_lossy(&invocation.result.stderr);
    assert!(
        stderr.contains("could not be evaluated by the adapter"),
        "stderr: {stderr}"
    );
}

#[test]
fn still_refuses_to_start_when_the_adapter_raises_on_the_original() {
    // Skipping applies to reduction only. A broken adapter must not be able to
    // produce a bundle from an input it could never evaluate.
    let mut configuration = config();
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "raise SystemExit(3)".to_owned(),
        "{input}".to_owned(),
    ];
    let invocation = invoke_with_export(INPUT, &configuration);

    assert!(!invocation.result.status.success());
    assert!(
        String::from_utf8_lossy(&invocation.result.stderr).contains("oracle exited unsuccessfully")
    );
    assert!(!invocation.output_path.exists());
    assert!(!invocation.export_path.expect("export directory").exists());
}

#[test]
fn reports_a_signed_size_change_when_the_candidate_is_not_smaller() {
    // The oracle only reproduces while a marker key survives, so nothing can be
    // removed and pretty-printing alone can outweigh a compact input. That must
    // be reported honestly rather than hidden behind a saturating subtraction.
    let input = r#"{"keep":1,"records":[{"id":"u-a"},{"id":"u-a"}]}"#;
    let mut configuration = config();
    // Reproduces only while the marker key survives *and* the ids still collide,
    // so nothing in the document can be removed.
    configuration.oracle.args = vec![
        "-c".to_owned(),
        "import json,sys\nd=json.load(open(sys.argv[1]))\nif 'keep' not in d:\n print(json.dumps({'outcome':'not_reproduced'}))\nelse:\n try:\n  ids=[r['id'] for r in d['records']]\n except (KeyError,TypeError):\n  print(json.dumps({'outcome':'invalid_candidate'}))\n else:\n  print(json.dumps({'outcome':'target_failure','target':'import-duplicate-id'} if len(ids)!=len(set(ids)) else {'outcome':'not_reproduced'}))"
            .to_owned(),
        "{input}".to_owned(),
    ];
    let configuration = {
        let mut configuration = configuration;
        configuration.replacements = ["/records/0/id", "/records/1/id"]
            .into_iter()
            .map(|path| Replacement::new(path, json!("SAFE")))
            .collect();
        configuration
    };

    let invocation = invoke(input, &configuration);
    assert!(
        invocation.result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&invocation.result.stderr)
    );

    let report: Value = serde_json::from_slice(&invocation.result.stdout).expect("JSON report");
    let delta = report["delta_bytes"].as_i64().expect("signed delta");
    let original = report["original_bytes"].as_u64().expect("original");
    let output = report["output_bytes"].as_u64().expect("output");
    assert_eq!(delta, output as i64 - original as i64);
    assert_eq!(report["smaller_than_input"], delta < 0);
    // The candidate is genuinely not smaller here, and the report must say so.
    assert!(delta > 0, "expected growth, got {delta}");
    assert_eq!(report["smaller_than_input"], false);
    // And the user is told, rather than left to trust a `removed_bytes: 0`.
    let stderr = String::from_utf8_lossy(&invocation.result.stderr);
    assert!(
        stderr.contains("the candidate is not smaller than the input"),
        "stderr: {stderr}"
    );
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

#[test]
fn reduces_a_bug_that_only_a_conjunction_of_two_filters_reproduces() {
    let invocation = invoke_fixture("search-filter-overlap");
    assert_success(&invocation);

    let report: Value = serde_json::from_slice(&invocation.result.stdout).expect("JSON report");
    let candidate: Value =
        serde_json::from_slice(&fs::read(&invocation.output_path).expect("sanitized candidate"))
            .expect("valid candidate JSON");

    assert_eq!(report["status"], "target_failure_preserved");
    assert_eq!(report["target"], "search-filter-overlap");
    assert_eq!(report["budget_exhausted"], false);
    assert_eq!(report["untestable_candidates"], 0);
    assert_eq!(report["smaller_than_input"], true);

    // Only the two clauses on the same field remain, and the payloads the
    // adapter never reads are gone. A single filter, or a pair on different
    // fields, would mean the conjunction was lost.
    assert_eq!(
        candidate,
        json!({
            "request": {
                "query": {
                    "filters": [
                        {"kind": "range", "field": "created_at"},
                        {"kind": "terms", "field": "created_at"}
                    ]
                }
            }
        })
    );
}

#[test]
fn fixture_bundle_hides_a_secret_embedded_in_a_longer_string_and_a_numeric_one() {
    let invocation = invoke_fixture("search-filter-overlap");
    // The input carries `svc-account-joão-bot`, an unrelated identifier that
    // merely starts with the protected actor name. Reaching a bundle at all
    // proves the default `exact` rule did not report it as a false positive.
    assert_success(&invocation);

    let bundle = read_bundle(&invocation);
    for protected in [
        "acme-corp-tenant",
        "svc-account-joão",
        "4820117",
        "trace-9f3a2b7c-secret",
        // Declared with `match = "contains"`, so the fragment must not survive
        // inside the longer `trace_note` string either.
        "cursor-opaque-secret",
    ] {
        assert!(
            !bundle.contains(protected),
            "protected value survived in the review bundle: {protected}"
        );
    }

    assert_eq!(
        fs::read(&invocation.input_path).expect("original input"),
        include_str!("../examples/search-filter-overlap/input.json").as_bytes()
    );
}

/// Runs a fixture adapter directly against a candidate, to confirm the artifact
/// Ghostcase wrote still reproduces on its own.
fn ask_adapter(fixture: &str, candidate: &std::path::Path) -> Value {
    let adapter = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(fixture)
        .join("oracle.py");
    let output = Command::new("python3")
        .arg(&adapter)
        .arg("--input")
        .arg(candidate)
        .output()
        .expect("run the fixture adapter");
    serde_json::from_slice(&output.stdout).expect("adapter protocol response")
}

#[test]
fn reduces_a_real_reproducible_jq_precision_bug() {
    // The adapter runs the real jq binary, so a missing jq has to be a clear
    // failure rather than a confusing protocol error.
    Command::new("jq")
        .arg("--version")
        .output()
        .expect("this test needs the jq binary on PATH");

    let invocation = invoke_fixture("settlement-precision");
    assert_success(&invocation);

    let report: Value = serde_json::from_slice(&invocation.result.stdout).expect("JSON report");
    let candidate: Value =
        serde_json::from_slice(&fs::read(&invocation.output_path).expect("sanitized candidate"))
            .expect("valid candidate JSON");

    assert_eq!(report["status"], "target_failure_preserved");
    assert_eq!(report["target"], "jq-integer-precision-loss");
    assert_eq!(report["budget_exhausted"], false);
    assert_eq!(report["smaller_than_input"], true);

    // The exact values chosen depend on the jq version, so assert the
    // invariants that make the candidate minimal rather than the numbers.
    let events = candidate["events"].as_array().expect("an events array");
    assert!(
        events.len() >= 2,
        "the bug needs a sum, so one event cannot reproduce it: {events:?}"
    );
    assert!(
        events.iter().all(
            |event| event.as_object().map(|object| object.len()) == Some(2)
                && event["account"].is_string()
                && event["amount"].is_i64()
        ),
        "each surviving event should keep only the account and the amount: {events:?}"
    );
    assert_eq!(
        candidate.as_object().map(|object| object.len()),
        Some(1),
        "everything except the events array should be gone: {candidate}"
    );

    // The written artifact must reproduce on its own, not only through the run
    // that produced it.
    assert_eq!(
        ask_adapter("settlement-precision", &invocation.output_path),
        json!({"outcome": "target_failure", "target": "jq-integer-precision-loss"})
    );

    let bundle = read_bundle(&invocation);
    for protected in [
        "stl-2026-09-30-secret",
        "financeiro@acomex.example",
        "BR97-0000-1234-5678-9012",
    ] {
        assert!(
            !bundle.contains(protected),
            "protected value survived in the review bundle: {protected}"
        );
    }

    assert_eq!(
        fs::read(&invocation.input_path).expect("original input"),
        include_str!("../examples/settlement-precision/input.json").as_bytes()
    );
}
