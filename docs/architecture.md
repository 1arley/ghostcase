# Ghostcase — initial architecture

Status: technical proposal for the first prototype.
Date: 2026-09-25.

## Objective

Transform a JSON input that reproduces a bug into a smaller example with
protected fields replaced, while preserving a user-defined target failure.
The result is evidence limited to the test performed, not proof of universal
anonymization or causal equivalence between two bugs.

## Selected stack

**Rust, edition 2024, one Cargo package containing a library and a CLI.**

| Component | Choice | Rationale |
| --- | --- | --- |
| CLI | `clap` | Typed arguments, consistent help and validation |
| Data model and reports | `serde` + `serde_json` | Explicit structures and interoperable output |
| Configuration | TOML with `toml` | Concise, versionable configuration without executable code |
| Temporary files | `tempfile` | Candidates kept separate from the input file |
| Execution | `std::process`, with platform-specific process control | Run the actual test without an implicit shell |
| Tests | `cargo test` and integration fixtures | Verify complete CLI behavior |
| Distribution | Binaries on GitHub Releases | Users do not need to install a language runtime |
| CI | GitHub Actions | Formatting, linting, tests and platform-specific artifacts |

The core is synchronous and evaluates one candidate at a time. Test execution
is expected to dominate the cost. Parallelism only makes sense after measuring
costs and demonstrating that executions do not interfere with each other.

Rust was selected for distribution, control over data representation and
modeling of execution states. Go is a viable alternative with a lower initial
development cost. Python would favor algorithm experimentation; TypeScript
would favor integration with the web ecosystem. Mixing these languages now
would add installation, packaging and maintenance work without a proven benefit.

The reproduction command can use any language. A Python, Java, Go or JavaScript
application does not need to be converted to use Ghostcase.

## Project language

Use English for documentation, code identifiers, comments, test names, CLI help,
errors, reports, configuration descriptions and project collaboration artifacts
such as commit messages, issues and pull requests. Keep protocol values and
configuration keys in English. Preserve non-English fixture content whenever
it is relevant to the behavior being tested, particularly Unicode bugs.

## First prototype contract

- Input: a JSON file, configuration and a trusted local command.
- Users identify protected fields with exact JSON Pointers (RFC 6901).
- Explicit replacements are applied together, before reduction.
- Related IDs can receive the same replacement declared at multiple paths.
  The first version does not infer relationships or personal data.
- The command receives the candidate path as a separate argument.
- Output: a reduced candidate, a test recipe and a report without original values.
- First supported environment: Linux; macOS follows process-control validation.
  Windows requires equivalent subprocess handling and tests.

A recipe may depend on the user's project and installed tools. Therefore, the
initial bundle must not be advertised as a self-contained reproducer.

## Recognizing the correct failure

Users write a small test adapter that receives the candidate file and returns
exactly one JSON object on stdout:

```json
{"outcome":"target_failure","target":"import-duplicate-id"}
```

The other outcomes are `not_reproduced` and `invalid_candidate`.
`target` must match the configured identifier. The adapter exits with code zero
when producing a valid result, including when it reproduces the bug. Invalid
output, an unexpected exit code, a timeout or an execution failure are runner
errors and never count as preservation of the target failure.

The adapter distinguishes the bug from validation or environment errors. It can
check a specific exception, a state condition or an assertion. Ghostcase does
not assume a nonzero exit code represents the bug. Result objects are restricted
to protocol fields; raw logs are excluded from the export report.

## Workflow

1. Validate configuration and JSON compatibility while preserving the original file.
2. Run the original more than once and require the same target failure.
3. Replace protected fields as a single transaction; verify the failure again.
4. Reduce the sanitized candidate in deterministic order, accepting only
   transformations that preserve the target failure.
5. Run the final candidate again and generate a report for review.
6. Explicitly export the reviewed artifact to a new destination.

If replacement prevents reproduction, the process ends without producing a
bundle ready to share. There is no fallback that exports the original.
Repeated observations of the same failure provide a basic instability check,
not a guarantee of determinism.

## Reduction and data integrity

Start with structural delta debugging: remove groups of array elements and
object properties, shrinking the groups when necessary. Repeat passes until
no reduction is accepted or the execution budget is exhausted.

Replacements happen before removing elements, so JSON Pointer indices still
refer to the correct original fields. Reduction only removes data from the
replaced candidate; it never reintroduces original values.

Do not simplify strings or numbers in the first version. Each attempted
transformation must decrease candidate size. The report indicates whether the
budget was exhausted; it does not promise the smallest possible example.

Preserve key order and number representation using the appropriate `serde_json`
features. Explicitly reject duplicate keys: silently converting them into a map
could erase the bug condition. Verify that initial serialization still reproduces
the failure. Bugs depending on whitespace, escapes or the original lexical
representation are outside the initial structural scope and must be reported
as incompatible.

## Execution limits and confidentiality

- Invoke the program with an argument vector, without shell command interpolation.
- Keep candidates and temporary logs separate from the input and restrict access
  through operating-system permissions. A temporary directory is not a security sandbox.
- The command is trusted user code and can access files and the network with
  the user's permissions. Container isolation is a later step.
- Enforce a per-attempt timeout, a total execution budget and an output limit.
- Terminate the process group on timeout; test child and grandchild processes too.
- The adapter must reset its own state between attempts.
- Do not automatically copy the repository, environment, logs or original data
  into the sharing bundle.
- Do not include a reversible replacement table, hashes of original values or
  original values in the report.
- Undeclared fields may still contain sensitive information. Human review of
  the candidate and recipe remains necessary.

## Initial layout

```text
src/
  main.rs       # arguments, presentation and exit codes
  lib.rs        # public execution workflow
  config.rs     # contract and validation
  runner.rs     # processes, limits and adapter protocol
  transform.rs  # replacements and structural reduction
  report.rs     # results and export
tests/
  fixtures/     # synthetic data and controlled adapters
  cli.rs        # end-to-end tests
```

Create modules as the first workflow requires them, without a plugin framework
or a multi-package workspace at this stage. The existing `Cargo.toml` and
`src/main.rs` still contain the `idk7` scaffold and were not changed by this decision.

## First milestone and acceptance criteria

The first test uses a fictional importer: two entries sharing an ID trigger a
failure, while many extra records are irrelevant. Protected fields contain
only known synthetic values.

| Case | Required result |
| --- | --- |
| IDs replaced consistently | The target failure persists |
| Irrelevant records removed | The candidate shrinks and the failure persists |
| Adapter produces a different failure | The transformation is not accepted |
| Replacement eliminates the bug | No export is considered ready |
| Timeout or stuck child process | The attempt ends without orphan processes |
| Original does not reproduce the bug | Execution stops before reduction |
| Input contains a duplicate key | Explicit error without silent data loss |
| Large number and ordered keys | No silent rounding or reordering |
| Search for protected values in artifacts | No declared original value appears |
| Complete execution | The input file remains byte-for-byte unchanged |

Next, validate against three reproducible real-world bugs, measuring size
before and after, execution count, total time and configuration effort. The
initial goal is to prove the workflow's usefulness and correctness, rather
than publish savings percentages.

## Future decisions

- Confirm the name and license before the first publication; recommendation: MIT.
- Package Linux and macOS binaries as each platform passes its tests.
- Add assisted detection of sensitive fields only after validating the explicit
  workflow. Probabilistic detectors do not authorize export automatically.
- Consider agent and GitHub Actions integrations after the CLI works.

## References

- https://github.com/renatahodovan/picire — delta debugging.
- https://github.com/data-privacy-stack/presidio — PII identification and replacement.
- https://docs.rs/serde_json/ — JSON representation.
- https://docs.rs/clap/ — CLI.
- https://docs.rs/tempfile/ — temporary files.
- https://www.rfc-editor.org/rfc/rfc6901 — JSON Pointer.
