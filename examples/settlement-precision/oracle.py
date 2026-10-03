#!/usr/bin/env python3

"""Real-world oracle: a settlement total that jq computes wrong.

This models an actual, reproducible defect in a real tool rather than a
synthetic one invented for Ghostcase. A settlement service runs a jq program
to total the captured amounts per account, and jq does not keep every integer
exact: a literal is echoed unchanged, but arithmetic converts to an IEEE-754
double. Two values that sum to more than 2**53 therefore come back rounded.

Reproduced with jq 1.8.2:

    $ echo '[1725884862351626139]' | jq -c 'map(.) | add'
    1725884862351626139
    $ echo '[1725884862351626139,1725884862351626140]' | jq -c 'map(.) | add'
    3451769724703252000
    $ python3 -c 'print(1725884862351626139 + 1725884862351626140)'
    3451769724703252279

That is the whole family of "jq silently corrupts my IDs" reports: database
bigints, snowflake identifiers and ledger amounts in cents all exceed 2**53,
and the total that reaches the settlement file is wrong while every
individual value still looked correct in the input.

The adapter therefore runs the real jq binary and compares its totals against
exact integer arithmetic. It does not simulate the rounding; if jq ever fixes
this, the adapter stops reporting the failure, which is the correct outcome.
"""

import argparse
import json
import subprocess
import sys

TARGET = "jq-integer-precision-loss"

# The reconciliation program the service actually runs.
PROGRAM = (
    "[.events | group_by(.account)[]"
    " | {account: .[0].account, total: (map(.amount) | add)}]"
)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    args = parser.parse_args()

    try:
        with open(args.input, encoding="utf-8") as source:
            document = json.load(source)
    except (OSError, UnicodeError, json.JSONDecodeError):
        print(json.dumps({"outcome": "invalid_candidate"}, separators=(",", ":")))
        return 0

    events = document.get("events") if isinstance(document, dict) else None
    well_formed = isinstance(events, list) and all(
        isinstance(event, dict)
        and isinstance(event.get("account"), str)
        and isinstance(event.get("amount"), int)
        and not isinstance(event.get("amount"), bool)
        for event in events
    )
    if not well_formed:
        outcome = {"outcome": "invalid_candidate"}
        print(json.dumps(outcome, separators=(",", ":")))
        return 0

    # Exact reference totals, computed with Python's arbitrary precision ints.
    exact: dict[str, int] = {}
    for event in events:
        exact[event["account"]] = exact.get(event["account"], 0) + event["amount"]

    try:
        computed = subprocess.run(
            ["jq", "-c", PROGRAM, args.input],
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        )
    except FileNotFoundError:
        print("the adapter needs the jq binary on PATH", file=sys.stderr)
        return 2
    except (subprocess.SubprocessError, ValueError):
        print(json.dumps({"outcome": "invalid_candidate"}, separators=(",", ":")))
        return 0

    try:
        reported = {
            group["account"]: group["total"] for group in json.loads(computed.stdout)
        }
    except (json.JSONDecodeError, KeyError, TypeError):
        print(json.dumps({"outcome": "invalid_candidate"}, separators=(",", ":")))
        return 0

    wrong = any(reported.get(account) != total for account, total in exact.items())
    outcome = (
        {"outcome": "target_failure", "target": TARGET}
        if wrong
        else {"outcome": "not_reproduced"}
    )
    print(json.dumps(outcome, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())