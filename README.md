# Ghostcase

Ghostcase turns a JSON input that triggers a bug into a smaller, safer example
that still triggers the same failure.

It replaces fields you mark as protected, then removes unnecessary JSON values
one at a time. After every change, Ghostcase asks your local test adapter whether
the exact target failure still happens. Original inputs stay untouched; the
sanitized output is written only to the new path you choose.

Ghostcase is an early prototype. It supports structural JSON reduction on Linux.
It does not detect sensitive fields automatically, guarantee anonymization, or
make a self-contained reproducer from an arbitrary project.

## Build

Requires Rust and Cargo.

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
record pair. Its JSON summary is printed to stdout.

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
not infer relationships between fields. Before exporting, it also checks that
protected string values do not appear inside other JSON strings. If the same
protected value occurs elsewhere, declare and replace each occurrence first.

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

Run the adapter as trusted local code. It inherits your filesystem and network
permissions; Ghostcase's temporary directory is not a security sandbox. Review
the final candidate and test recipe before sharing them. Protected values at
unlisted JSON paths and sensitive details in adapter code need separate review.

See [docs/architecture.md](docs/architecture.md) for the design and current
limitations.

## Development status

- [x] Rust CLI scaffold
- [x] Linux oracle runner and strict JSON result protocol
- [x] Explicit JSON Pointer replacements
- [x] Deterministic, budgeted structural reduction
- [x] Synthetic importer example
- [x] Synthetic Unicode validation fixture
- [x] Integration test suite
- [x] Linux CI
- [x] Review-ready export bundle
- [ ] macOS process-group support
