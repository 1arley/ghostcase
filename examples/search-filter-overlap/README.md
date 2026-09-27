# Search filter overlap fixture

This is a **synthetic** case, not a claim about a real project. It models a
search query builder that applies two clauses to the same field: when a `range`
filter and a `terms` filter name the same field, the later clause replaces the
earlier one and the result set silently widens. The adapter reports
`search-filter-overlap`.

The input keeps the whole `created_at` range and terms payloads, but the adapter
reads only `kind` and `field`, so reduction is expected to strip `from`, `to` and
`values` as well as the five unrelated filters.

From the repository root:

```sh
cargo run --release -- minimize \
  --input examples/search-filter-overlap/input.json \
  --config examples/search-filter-overlap/ghostcase.toml \
  --output /tmp/ghostcase-search.json \
  --export-dir /tmp/ghostcase-search-review
```

The expected result is a preserved target failure and a candidate holding only
the two overlapping `created_at` filters. If the reducer ever returns a single
filter, or a pair of filters on *different* fields, the conjunction was lost.

## What this fixture adds over the other two

| Behavior | Where it is exercised |
| --- | --- |
| The bug needs a *conjunction* of two elements, not a duplicate value | `filters` must shrink to exactly the overlapping pair |
| The adapter reports a **second, different** failure | A candidate keeping `cursor` and a non-zero `offset` yields `search-cursor-offset-conflict`, which must be rejected rather than accepted as progress |
| A protected value survives inside a **longer string** | `/request/query/trace_note` embeds the cursor secret, so the cursor declares `match = "contains"` and the second location is declared too |
| A **numeric** protected value | `/request/account_id` uses `match = "contains"`, the strict document-wide rule for numbers that identify somebody |
| A false positive that must **not** be reported | `/context/audit/requested_by` is `svc-account-joão-bot`, an unrelated identifier that merely starts with the protected actor name. The default `exact` rule leaves it alone |

The last row is the reason the actor keeps the default rule: substring matching
would flag `svc-account-joão-bot` and stop the run. This fixture therefore fails
loudly if the default ever becomes greedy.

## What it does not prove

The shape and the defect are modeled on a real class of bug, but the adapter is
still a synthetic one. Nothing here shows how Ghostcase behaves against a real
application, a real framework or a non-trivial adapter.
