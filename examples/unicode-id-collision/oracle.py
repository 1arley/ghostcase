#!/usr/bin/env python3

"""Synthetic Unicode-normalization oracle for Ghostcase."""

import argparse
import json
import sys
import unicodedata


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
        normalized_ids = [unicodedata.normalize("NFC", value) for value in ids]
        raw_duplicate = len(ids) != len(set(ids))
        canonical_collision = len(normalized_ids) != len(set(normalized_ids))
        collision = canonical_collision and not raw_duplicate
        outcome = (
            {"outcome": "target_failure", "target": "unicode-id-collision"}
            if collision
            else {"outcome": "not_reproduced"}
        )
    print(json.dumps(outcome, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
