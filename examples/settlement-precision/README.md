# Settlement precision fixture

This fixture is the real-world validation case. Unlike the other three, the
defect is not invented for Ghostcase: it is a reproducible bug in **jq**, hit by
any pipeline that totals values stored as JSON numbers.

## The real bug

jq does not keep every integer exact. A literal is echoed unchanged, but any
arithmetic converts to an IEEE-754 double, so a total above 2**53 comes back
rounded. Reproduced with jq 1.8.2:

```sh
$ echo '[1725884862351626139]' | jq -c 'map(.) | add'
1725884862351626139
$ echo '[1725884862351626139,1725884862351626140]' | jq -c 'map(.) | add'
3451769724703252000
$ python3 -c 'print(1725884862351626139 + 1725884862351626140)'
3451769724703252279
```

Database bigints, snowflake identifiers and ledger amounts in cents all exceed
2**53. This is why a settlement total can be wrong while every individual value
in the input still looks correct.

The adapter runs the **real jq binary** and compares its per-account totals
against exact Python integer arithmetic. It does not simulate the rounding, so if
jq ever fixes this the adapter stops reporting a failure, which is the correct
outcome for a regression test.

## Running it

The adapter needs `jq` on `PATH`. From the repository root:

```sh
cargo run --release -- minimize \
  --input examples/settlement-precision/input.json \
  --config examples/settlement-precision/ghostcase.toml \
  --output /tmp/ghostcase-settlement.json \
  --export-dir /tmp/ghostcase-settlement-review
```

## Measured result

| Metric | Value |
| --- | --- |
| Input | 3734 bytes, 20 events across 6 accounts |
| Candidate | 162 bytes, 2 events |
| Reduction | −3572 bytes, 95.7% |
| Adapter executions | 54 (50 in reduction) |
| Wall clock | ~3.6 s, ~66 ms per execution |
| Configuration effort | 9 lines, 3 replacements |

The reduced candidate is:

```json
{
  "events": [
    {"account": "acc-epsilon", "amount": -1},
    {"account": "acc-epsilon", "amount": 9007199254740993}
  ]
}
```

jq totals that group to `9007199254740991`; the exact sum is `9007199254740992`.
The minimal case is genuinely non-obvious:

- **One event is never enough.** jq returns a single-element literal unchanged,
  so the bug needs at least two values to reach the addition. The adapter
  confirms this: the same document with one event reports `not_reproduced`.
- **A merely large value is not enough.** `9007199254740993` alone is wrong
  only after it is converted. The winning pair is `-1` and `9007199254740993`,
  whose exact sum is `2**53`, a value that *is* exactly representable. The
  corruption comes from rounding `9007199254740993` down to `2**53` before the
  subtraction, landing one unit short. A reducer that only looked for large
  numbers would keep a three-event group instead.

## Scope of the validation

What this proves: Ghostcase reduces a real, currently reproducible defect in a
real third-party binary, from a realistic payload, to a two-field reproducer,
without leaking the declared protected values.

What it does not prove: the adapter here still uses a purpose-written Python
wrapper. It has not been pointed at an application's own test suite, and the
reduced candidate has not been confirmed to be minimal among all inputs, only
minimal among the transformations Ghostcase attempts.