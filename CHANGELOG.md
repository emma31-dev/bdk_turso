# Changelog

Notable changes to `bdk_turso` are documented here.

## [Unreleased]

### Changed

- `Store::options()` now returns `None` for stores created with `Store::from_database`, where the database's opening options are unknown. Store constructors retain their known options, including defaults.

## [0.2.3]

### Changed

- Component-level store read and write methods now open and manage their own connections and transactions; callers no longer pass transactions to these methods.
- Wallet changeset writes remain atomic, and changeset reads use a consistent transaction across components.

## [0.2.2]

### Changed

- Expanded API documentation for store methods and wallet persistence.

### Fixed

- Selected the standard-library implementation for the `branches` dependency to avoid its incompatible nightly-only code path in documentation builds.

## [0.2.1]

### Changed

- Made `Options` fields private and added builder methods to configure experimental options and encryption.

## [0.2.0]

### Added

- Configurable experimental Turso options, disabled by default, and encryption options.
- Continuous integration checks for stable Rust and Rust 1.88.
- Contribution guidelines and GitHub issue and pull request templates.

## [0.1.0]

### Added

- Initial release of the async Turso persistence backend for `bdk_wallet`, implementing `AsyncWalletPersister`.
- A SQLite-compatible persistence schema and migration, with wallet-state persistence and read/write methods.
- A wallet example and persistence tests, including a check that the database can be read by `bdk_sqlite`.
