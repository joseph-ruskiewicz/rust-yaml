# Vendored YAML Test Suite

The `data/` directory contains input cases vendored from the official
[YAML Test Suite](https://github.com/yaml/yaml-test-suite), the community
conformance suite referenced by the yaml2 design spec as the compliance bar.

- **Source:** https://github.com/yaml/yaml-test-suite (`data` branch)
- **Commit:** `6ad3d2c62885d82fc349026c136ef560838fdf3d`
- **License:** MIT, © the YAML Test Suite authors.

Only the files this crate needs are vendored: each case directory keeps its
`in.yaml` (the input) and, for cases that must be rejected, an empty `error`
marker file. The suite's `in.json`, `out.yaml`, `test.event`, and `===` files
are intentionally omitted.

## How it is used

`tests/yaml_test_suite.rs` runs every case through `yaml2::parse_documents` and
asserts the success/failure outcome: error-marked cases must be rejected, all
others must parse. It is a ratcheting gate — see the test's module docs. The
list of currently-failing cases lives in that file's `KNOWN_FAILURES`.

## Refreshing the corpus

```sh
git clone --depth 1 --branch data https://github.com/yaml/yaml-test-suite /tmp/yts
rsync -a --exclude='.git' --include='*/' --include='in.yaml' --include='error' \
  --exclude='*' --prune-empty-dirs /tmp/yts/ yaml2/tests/yaml_test_suite/data/
```

Then update the commit SHA above and reconcile `KNOWN_FAILURES`.
