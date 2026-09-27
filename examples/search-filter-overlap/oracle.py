#!/usr/bin/env python3

"""Synthetic search API oracle. Emits only the documented Ghostcase protocol.

The modelled bug is a query builder that applies two clauses to the same
field: when a ``range`` filter and a ``terms`` filter name the same field, the
later clause replaces the earlier one and the result set silently widens.

A second, unrelated defect is modelled on purpose. When a request carries both
a cursor and a non-zero offset the paginator reports a different failure, and
the adapter must name that other target. Returning it is not evidence that
``search-filter-overlap`` was preserved, so Ghostcase has to reject the
candidate instead of accepting the reduction that produced it.
"""

import argparse
import json
import sys

TARGET = "search-filter-overlap"
OTHER_TARGET = "search-cursor-offset-conflict"


def is_filter(value: object) -> bool:
    return (
        isinstance(value, dict)
        and isinstance(value.get("kind"), str)
        and isinstance(value.get("field"), str)
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

    request = document.get("request") if isinstance(document, dict) else None
    query = request.get("query") if isinstance(request, dict) else None
    filters = query.get("filters") if isinstance(query, dict) else None

    if not isinstance(filters, list) or not all(is_filter(item) for item in filters):
        outcome = {"outcome": "invalid_candidate"}
    else:
        ranged = {item["field"] for item in filters if item["kind"] == "range"}
        termed = {item["field"] for item in filters if item["kind"] == "terms"}
        cursor = query.get("cursor")
        offset = query.get("offset")
        if ranged & termed:
            outcome = {"outcome": "target_failure", "target": TARGET}
        elif isinstance(cursor, str) and isinstance(offset, int) and offset > 0:
            outcome = {"outcome": "target_failure", "target": OTHER_TARGET}
        else:
            outcome = {"outcome": "not_reproduced"}

    print(json.dumps(outcome, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
