# Ghostcase — initial architecture

Status: implemented prototype. This document is the design of record; where an
earlier revision described intended behavior that the code did not have, the
text below has been corrected to match the code.
Date: 2026-09-25. Last revised: 2026-09-27.

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
- A protected string value must not remain, even as a substring of another
  JSON string, when the replacement declares `match = "contains"`. With the
  default `match = "exact"` the same whole string must not remain, anywhere.
- The command receives the candidate path as a separate argument.
- Output: a reduced candidate, a test recipe and a report without original values.
- Supported environments: Linux and macOS. Both isolate each oracle run in its
  own process group and are validated by the same CI matrix and integration
  tests. Windows requires equivalent subprocess handling and tests.

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
4. Reduce the sanitized candidate by delta debugging, accepting only
   transformations that preserve the target failure and skip the ones the
   adapter cannot evaluate.
5. Run the final candidate again and generate a report for review.
6. Explicitly export the reviewed artifact to a new destination.

If replacement prevents reproduction, the process ends without producing a
bundle ready to share. There is no fallback that exports the original.
Repeated observations of the same failure provide a basic instability check,
not a guarantee of determinism.

The optional review bundle contains `candidate.json`, `report.json`, and
`recipe.json`. It excludes original protected values, logs, environment data,
and the replacement table. The recipe identifies the adapter but does not make
the bundle self-contained; a reviewer must inspect the adapter arguments and
undeclared fields before sharing it.

## Reduction and data integrity

Start with structural delta debugging: cut each container's children into groups
whose granularity doubles until a group can be dropped, then recurse into the
surviving groups. Dropping N of M elements costs O(log N) adapter runs rather
than one run per element. Each container in the tree is reduced in turn, and the
test callback always receives a complete document, never a fragment. Repeat
passes until no reduction is accepted or the execution budget is exhausted.

Reduction is tri-state. `target_failure` preserves, `not_reproduced`,
`invalid_candidate` and a mismatched target reject, and an execution error is
neither: reduction removes the fields an adapter reads, so a candidate it cannot
evaluate is expected. Such a candidate is skipped, never accepted, and becomes
the group that delta debugging shrinks into next. Execution errors stay fatal
during the verification runs that bracket reduction, because a bundle must never
come from an input the adapter could not evaluate.

Replacements happen before removing elements, so JSON Pointer indices still
refer to the correct original fields. Reduction only removes data from the
replaced candidate; it never reintroduces original values.

Do not simplify strings or numbers in the first version. Each attempted
transformation must decrease candidate size, measured on the whole document, so
reduction can never be the reason a candidate grew. The report states the signed
size change against the input file: the data shrinks monotonically, but the
candidate is written pretty-printed, so an already minimal compact input can
still produce a larger file. That is reported and warned about, not hidden behind
a clamped subtraction. The report indicates whether the budget was exhausted; it
does not promise the smallest possible example.

Preserve key order and exact numeric values using the appropriate `serde_json`
features. Serialization may normalize number spelling, whitespace and escapes;
bugs depending on those lexical details are outside the initial structural scope
and must be reported as incompatible. Explicitly reject duplicate object keys:
silently converting them into a map could erase the bug condition. Verify that
initial serialization still reproduces the failure.

## Protected value detection

Each replacement declares how its original value is detected.

- `exact` (default): a protected string must not survive as the same whole string
  anywhere in the candidate. A non-string value is only guarded at its declared
  location, because a bare number or boolean identifies nobody and a
  document-wide match would reject ordinary data.
- `contains`: the strict rule for real secrets. The value must also not appear
  inside a longer string or as an object key, and non-string values are compared
  against the whole document.

Substring matching is opt-in because it reports false positives on ordinary
identifier schemes: protecting `user-1` also matches the unrelated `user-10` and
`user-100`, with no way to resolve the conflict. Generated bundles are always
scanned with the strict rule, because Ghostcase writes those itself and a false
positive there would only block a bundle that is already safe.

## Execution limits and confidentiality

- Invoke the program with an argument vector, without shell command interpolation.
- Keep candidates and temporary logs separate from the input and restrict access
  through operating-system permissions. A temporary directory is not a security sandbox.
- The command is trusted user code and can access files and the network with
  the user's permissions. Container isolation is a later step.
- Enforce a per-attempt timeout, a total execution budget and an output limit.
- Terminate the process group on timeout; test child and grandchild processes too.
  Linux and macOS share the same POSIX mechanism, so one test covers both. Exit is
  detected with `waitid(WNOWAIT)` so the child is still unreaped when the group is
  signalled: a reaped PID can be recycled, and the negative-PID kill would then
  reach an unrelated group.
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
  main.rs       # arguments, presentation, exit codes and the run workflow
  config.rs     # contract and validation
  json.rs       # strict parsing and protected value rules
  oracle.rs     # processes, limits and adapter protocol
  reduce.rs     # delta debugging over every container
tests/
  cli.rs        # end-to-end tests
examples/
  importer/             # synthetic duplicate-identifier fixture
  unicode-id-collision/ # synthetic Unicode validation fixture
```

Unit tests live beside the code they cover, in `#[cfg(test)]` modules, so the
strict parser, the protection rules and the reducer are tested directly rather
than only through the CLI. `serde_json` resolves the user's JSON Pointers, so
Ghostcase does not carry its own pointer parser. Create modules as the first
workflow requires them, without a plugin framework or additional Cargo packages
at this stage. The root `ghostcase` package owns the CLI and core
implementation.

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

- Package Linux and macOS binaries as each platform passes its tests. The release
  workflow in `.github/workflows/release.yml` does this on a `v*` tag, running
  the same format, lint and test gates as CI before it packages anything.
- Add assisted detection of sensitive fields only after validating the explicit
  workflow. Probabilistic detectors do not authorize export automatically.
- Consider agent and GitHub Actions integrations after the CLI works.

The license is MIT, in `LICENSE`. The dual `MIT OR Apache-2.0` declaration was
narrowed to match that file. `LICENSE` names `1arley`, the repository owner, as
the copyright holder; change it if the project changes hands.

## References

- https://github.com/renatahodovan/picire — delta debugging.
- https://github.com/data-privacy-stack/presidio — PII identification and replacement.
- https://docs.rs/serde_json/ — JSON representation.
- https://docs.rs/clap/ — CLI.
- https://docs.rs/tempfile/ — temporary files.
- https://www.rfc-editor.org/rfc/rfc6901 — JSON Pointer.
