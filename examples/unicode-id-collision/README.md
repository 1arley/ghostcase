# Unicode ID collision fixture

This is a **synthetic** validation case, not a claim about a real project. It
models an importer that compares raw IDs first and normalizes them to NFC later.
The first two IDs are different byte sequences but canonically equivalent, so
the adapter reports `unicode-id-collision`.

From the repository root:

```sh
cargo run --release -- minimize \
  --input examples/unicode-id-collision/input.json \
  --config examples/unicode-id-collision/ghostcase.toml \
  --output /tmp/ghostcase-unicode.json \
  --export-dir /tmp/ghostcase-unicode-review
```

The expected result is a preserved target failure with the two IDs replaced by
distinct, canonically equivalent safe values, unrelated fields removed, and no
original protected values in the review bundle. The fixture deliberately keeps
non-ASCII content because Unicode behavior is part of the case.
