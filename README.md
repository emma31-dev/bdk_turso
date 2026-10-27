# bdk_turso

An experimental persistence backend for `bdk_wallet` that stores wallet state in Turso.

[![Crate Info](https://img.shields.io/crates/v/bdk_turso.svg)](https://crates.io/crates/bdk_turso)
[![Wallet API Docs](https://img.shields.io/badge/docs.rs-bdk_turso-green)](https://docs.rs/bdk_turso)
[![CI Status](https://github.com/emma31-dev/bdk_turso/workflows/CI/badge.svg)](https://github.com/emma31-dev/bdk_turso/actions?query=workflow:CI)
[![MIT or Apache-2.0 Licensed](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](#license)

## Project status

`bdk_turso` is early-stage and experimental. It currently implements async wallet persistence, and its API may change.

## What is bdk_turso?

`bdk_turso` is a standalone persistence backend for [`bdk_wallet`](https://docs.rs/bdk_wallet), storing wallet state in [Turso](https://turso.tech/), an in-process, SQLite-compatible database written in Rust. It implements the `AsyncWalletPersister` trait behind the `wallet` feature, without changes to `bdk_wallet` or `bdk_persist`.

`bdk_turso` is forked from [`bdk_sqlite`](https://docs.rs/bdk_sqlite), with its persistence implementation adapted for Turso's async-first API. Its persistence tests are adapted from the test suite in [`bdk_redb`](https://docs.rs/bdk_redb).

## Why Turso?

### Native async persistence

Turso's async-first design allows `bdk_turso` to implement `AsyncWalletPersister` natively rather than wrapping blocking database operations in `spawn_blocking`. Turso is designed to support concurrent write scaling; this project makes no benchmark claims.

### SQLite compatibility

`bdk_turso` stays within Turso's SQLite-compatible feature subset and avoids Turso-only extensions. Unencrypted database files remain readable by `bdk_sqlite`; Turso-encrypted files require Turso to open.

## Usage

The API is not finalized; this illustrative example shows the intended shape:

```rust,ignore
use bdk_turso::Store;
use bdk_wallet::{bitcoin::Network, Wallet};

let mut store = Store::new("wallet.sqlite")?;
let wallet = Wallet::new_or_load(
    descriptor,
    change_descriptor,
    Network::Testnet,
    &mut store,
)?;
```

## Examples

- `cargo run --example wallet` syncs a wallet with a local Bitcoin Core regtest node.
- `cargo run --features psbt --example transaction` syncs a regtest wallet, stores and reloads an unsigned PSBT, confirms it matches, signs and broadcasts it, and persists the resulting wallet changeset. It requires `POLAR_RPC_USER`, `POLAR_RPC_PASS`, and a regtest `RECIPIENT_ADDRESS`. If the wallet has no spendable funds, it prints a receive address; fund it and mine 101 blocks before rerunning.

Both examples requires Polar.

## Feature flags

| Feature | Description |
| --- | --- |
| `wallet` | Enables the `AsyncWalletPersister` implementation. |
| `psbt` | Enables optional PSBT storage methods; also enables `wallet`. |

## Minimum Supported Rust Version (MSRV)

The MSRV is 1.88.0, based on the current resolved dependency graph.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.

## Contribution

Contributions are welcome. Please read the [contribution guide](CONTRIBUTING.md), the [BDK contributing guidelines](https://github.com/bitcoindevkit/bdk/blob/master/CONTRIBUTING.md), and the [BDK community standards](https://github.com/bitcoindevkit/bdk/community).
