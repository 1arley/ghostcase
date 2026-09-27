# Ghostcase

Ghostcase turns a JSON input that triggers a bug into a smaller, safer example
that still triggers the same failure.

It replaces fields you mark as protected, then removes unnecessary JSON values
one at a time. After every change, Ghostcase asks your local test adapter whether
the exact target failure still happens. Original inputs stay untouched; the
sanitized output is written only to the new path you choose.

Ghostcase is an early prototype. It supports structural JSON reduction on Linux
and macOS. It does not detect sensitive fields automatically, guarantee
anonymization, or make a self-contained reproducer from an arbitrary project.

## Build

Requires Rust and Cargo. The oracle runner supports Linux and macOS only.

```sh
cargo build --release
```

## Try the synthetic example

From the repository root, run:

```sh
cargo run --release -- minimize \
  --input examples/importer/input.json \
  --config examples/importer/ghostcase.toml \
  --output /tmp/ghostcase-minimized.json
```

The example contains duplicate protected IDs and unrelated data. The adapter
reports a target failure only while duplicate IDs are present. Ghostcase should
replace the IDs consistently, remove irrelevant data, and keep the duplicate
record pair. Its JSON summary is printed to stdout. The fields are:

| Field | Meaning |
| --- | --- |
| `status` | `target_failure_preserved` only when the target failure survived every check. |
| `executions` | Total adapter runs, including the fixed verification runs. |
| `reduction_runs` | Adapter runs spent inside delta debugging. |
| `original_bytes` / `output_bytes` | Size of the input file and of the written candidate. |
| `delta_bytes` | Signed `output_bytes - original_bytes`. Negative means smaller. |
| `smaller_than_input` | Whether the candidate actually beat the input. |
| `budget_exhausted` | Reduction stopped at `max_runs` and may not be minimal. |
| `untestable_candidates` | Candidates the adapter could not evaluate. Skipped, never kept. |

`delta_bytes` is signed on purpose. Reduction guarantees the *data* shrinks
monotonically, but the candidate is written pretty-printed, so a compact input
that was already minimal can still produce a larger file. Ghostcase reports that
and warns on stderr rather than hiding it behind a clamped `removed_bytes`.

To inspect the result:

```sh
cat /tmp/ghostcase-minimized.json
```

The output path must not already exist. This prevents accidental replacement
of an earlier reproduction.

To create a review bundle as well, add a new directory:

```sh
cargo run --release -- minimize \
  --input examples/importer/input.json \
  --config examples/importer/ghostcase.toml \
  --output /tmp/ghostcase-minimized.json \
  --export-dir /tmp/ghostcase-review
```

The bundle contains `candidate.json`, `report.json`, and `recipe.json`. The
bundle never includes original protected values, logs, environment data, or a
replacement table. It is not self-contained: review the oracle program and
arguments in `recipe.json` before sharing it. The export directory must not
already exist, and the candidate is still written to `--output`.

A second synthetic fixture covers canonically equivalent Unicode identifiers;
see [`examples/unicode-id-collision/README.md`](examples/unicode-id-collision/README.md).
A third one models a bug that only a *conjunction* of two filters reproduces,
and its adapter reports a second, different failure on purpose; see
[`examples/search-filter-overlap/README.md`](examples/search-filter-overlap/README.md).

## Configure an adapter

Create a TOML file:

```toml
target = "import-duplicate-id"
max_runs = 100
timeout_seconds = 10

[oracle]
program = "python3"
args = ["oracle.py", "--input", "{input}"]

[[replacements]]
path = "/records/0/id"
with = "EXAMPLE-USER"

[[replacements]]
path = "/records/1/id"
with = "EXAMPLE-USER"
```

Paths use JSON Pointer (RFC 6901). Point the same replacement value at each
location that shares an identifier or another protected value. Ghostcase does
not infer relationships between fields.

### How a protected value is detected

Each replacement accepts an optional `match` rule:

| `match` | Behavior |
| --- | --- |
| `exact` (default) | A protected **string** must not survive as the same whole string anywhere in the candidate. If it does, declare and replace each occurrence. |
| `contains` | The stricter rule for real secrets: the value must also not appear inside a longer string or as an object key. |

Non-string protected values are only checked at the location you declared. A bare
number or boolean identifies nobody, so matching it across the whole document
would reject ordinary data — use `match = "contains"` when a numeric value is
itself sensitive and must not survive anywhere.

Substring scanning is opt-in for a reason. With it enabled by default, protecting
`user-1` also matches the unrelated `user-10`, `user-100` and `user-1198`, and the
run stops with no way to resolve it. The default rule compares whole strings, so
sequential identifiers and prefixed names work as expected.

```toml
[[replacements]]
path = "/customer/email"
with = "user@example.com"
match = "contains"
```

The adapter receives the candidate path wherever `{input}` appears in its
argument list. It must print one JSON object and exit successfully:

```json
{"outcome":"target_failure","target":"import-duplicate-id"}
```

The other valid outcomes are `not_reproduced` and `invalid_candidate`. Returning
`target_failure` with another target does not preserve the configured bug. A
nonzero exit code, malformed response, timeout or output limit is an execution
error; Ghostcase will not count it as a reproduced bug. Keep adapters
deterministic and reset their state on every invocation.

An execution error is fatal while Ghostcase is **verifying** — on the original
input, after replacement, and on the final candidate. During **reduction** it is
not: reduction removes the very fields an adapter reads, so a candidate the
adapter cannot evaluate is expected. Such a candidate is skipped, never
accepted, and delta debugging narrows down the part of the document responsible.
Ghostcase reports how many were skipped and warns on stderr, so make adapters
answer `invalid_candidate` instead of raising.

Run the adapter as trusted local code. It inherits your filesystem and network
permissions; Ghostcase's temporary directory is not a security sandbox. Review
the final candidate and test recipe before sharing them. Protected values at
unlisted JSON paths and sensitive details in adapter code need separate review.

See [docs/architecture.md](docs/architecture.md) for the design and current
limitations.

## Development status

- [x] Rust CLI scaffold
- [x] Linux and macOS oracle runner and strict JSON result protocol
- [x] Explicit JSON Pointer replacements
- [x] Deterministic, budgeted structural reduction by delta debugging
- [x] Synthetic importer example
- [x] Synthetic Unicode validation fixture
- [x] Synthetic conjunctive-filter fixture whose adapter reports a second failure
- [x] Integration test suite
- [x] Unit tests for the JSON parser, protection rules and reducer
- [x] Linux and macOS CI
- [x] Review-ready export bundle
- [x] Release workflow publishing Linux and macOS binaries
- [ ] Windows subprocess handling
- [ ] Validation against real-world bugs

## Not yet validated

The three fixtures are synthetic and deliberately small: a few dozen lines of
Python each, checking duplicate identifiers, Unicode normalization and filter
overlap. The workflow has not been exercised against real bugs yet, so treat the
reduction quality and the adapter contract as unproven on anything but a
well-behaved adapter.
