#!/usr/bin/env python3

"""Real-world oracle: a lone surrogate reaching a UTF-8 encoder.

This models an actual, reproducible failure in real code rather than a
synthetic defect invented for Ghostcase. ``\\ud83d`` is a syntactically valid
JSON string escape, but it denotes a surrogate code point, which is not a
character and cannot be encoded. CPython parses it without complaint and then
raises as soon as the string reaches a real UTF-8 encoder:

    UnicodeEncodeError: 'utf-8' codec can't encode character '\\ud83d'
    in position 0: surrogates not allowed

That is the whole family of "UnicodeEncodeError on JSON input" reports: a
payload arrives, a validation layer accepts it, and the failure surfaces later
in a writer, a database column, an index or an HTTP response body. The walk
below is the minimum every one of those paths does, so the adapter reports the
same target whether the value is a string, an object key or a nested list item.

No parsing extension is invented here: the input is valid JSON, and the
adapter only uses the standard library.
"""

import argparse
import json
import sys

TARGET = "lone-surrogate-utf8"


def offending(node: object) -> bool:
    if isinstance(node, str):
        try:
            node.encode("utf-8")
        except UnicodeEncodeError:
            return True
        return False
    if isinstance(node, dict):
        # Real consumers iterate items(), so a surrogate used as a key is
        # reachable by exactly the same code path.
        return any(offending(key) or offending(value) for key, value in node.items())
    if isinstance(node, list):
        return any(offending(item) for item in node)
    return False


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

    outcome = (
        {"outcome": "target_failure", "target": TARGET}
        if offending(document)
        else {"outcome": "not_reproduced"}
    )
    print(json.dumps(outcome, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())