# Contributing to bdk_turso

Contributions are welcome. Please follow the BDK [Code of Conduct](https://github.com/bitcoindevkit/bdk/blob/master/CODE_OF_CONDUCT.md).

## Start with an issue

Open an issue before starting work, and wait for the project owner's feedback on the proposed change. Pull requests should address an existing issue and include a `Closes #<issue-number>` reference. This helps agree on scope before implementation and keeps changes coordinated.

Use the bug report or feature request issue form when opening an issue. For security-sensitive reports, do not post secrets, wallet data, or vulnerability details publicly.

## Tests

Tests are expected when a change alters runtime behavior, persistence semantics, or error handling. Documentation, formatting, and copy-only changes (including changing displayed text without changing program logic) do not need tests. Don't add tests solely to assert wording.

## Local checks

For code changes, run the checks relevant to your change. The CI workflow runs:

```sh
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo test --all-targets --all-features --locked
cargo check --no-default-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps --locked
```

Please include the checks you ran in your pull request description, and note any that were not applicable or could not be run.

## Benchmarks

The comparative benchmark suite is not run in CI and has no pass/fail thresholds. To reproduce a published result, run it on a quiet machine:

```sh
cargo bench --bench backends
```

Settings such as sample counts, batch sizes, writer counts, dataset shape, Turso options, and enabled backends are read from the optional `benches/config.toml`. Copy `benches/example.toml` to that path to customize a run; the local file is ignored by git.

See [BENCHMARK.md](BENCHMARK.md) for the workload, configuration options, methodology, limitations, and the procedure for publishing results before a release.

## Releases

Releases are published to crates.io and docs.rs by the `Publish` GitHub Actions workflow when a `v*` tag is pushed. See [RELEASING.md](RELEASING.md) for the release checklist, one-time trusted-publishing setup, and rollback procedure. Maintainers should follow that document rather than publishing manually.
