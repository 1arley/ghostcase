#!/usr/bin/env python3

"""Synthetic importer oracle. Emits only the documented Ghostcase protocol."""

import argparse
import json
import sys


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    args = parser.parse_args()

    try:
        with open(args.input, encoding="utf-8") as source:
            document = json.load(source)
    except (OSError, UnicodeError, json.JSONDecodeError):
        print(json.dumps({"outcome": "invalid_candidate"}))
        return 0

    records = document.get("records")
    if not isinstance(records, list) or not all(
        isinstance(record, dict) and isinstance(record.get("id"), str)
        for record in records
    ):
        outcome = {"outcome": "invalid_candidate"}
    else:
        ids = [record["id"] for record in records]
        duplicate = len(ids) != len(set(ids))
        outcome = (
            {"outcome": "target_failure", "target": "import-duplicate-id"}
            if duplicate
            else {"outcome": "not_reproduced"}
        )
    print(json.dumps(outcome, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
