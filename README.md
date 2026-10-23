# bdk_turso

An experimental persistence backend for `bdk_wallet` that stores wallet state in Turso.

[![Crate Info](https://img.shields.io/crates/v/bdk_turso.svg)](https://crates.io/crates/bdk_turso)
[![Wallet API Docs](https://img.shields.io/badge/docs.rs-bdk_turso-green)](https://docs.rs/bdk_turso)
[![CI Status](https://github.com/bitcoindevkit/bdk_turso/workflows/CI/badge.svg)](https://github.com/bitcoindevkit/bdk_turso/actions?query=workflow:CI)
[![MIT or Apache-2.0 Licensed](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](#license)

## Project status

`bdk_turso` is early-stage and experimental. The persistence implementation is not yet available, and its API may change.

## What is bdk_turso?

`bdk_turso` is a standalone persistence backend for [`bdk_wallet`](https://docs.rs/bdk_wallet), storing wallet state in [Turso](https://turso.tech/), an in-process, SQLite-compatible database written in Rust. It is intended to implement the `WalletPersister` and `AsyncWalletPersister` traits behind the `wallet` feature, without changes to `bdk_wallet` or `bdk_persist`.

The crate follows the storage-backend pattern used by [`bdk_sqlite`](https://docs.rs/bdk_sqlite) and [`bdk_redb`](https://docs.rs/bdk_redb). Unlike a synchronous backend adapted for async use, `bdk_turso` will use Turso's async-first API for native async persistence.

## Why Turso?

### Native async persistence

Turso's async-first design allows `bdk_turso` to implement `AsyncWalletPersister` natively rather than wrapping blocking database operations in `spawn_blocking`. Turso is designed to support concurrent write scaling; this project makes no benchmark claims.

### SQLite compatibility

`bdk_turso` will stay within Turso's SQLite-compatible feature subset and avoid Turso-only extensions. Database files are intended to remain readable by `bdk_sqlite`.

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

## Feature flags

| Feature | Description |
| --- | --- |
| `wallet` | Enables the `WalletPersister` and `AsyncWalletPersister` implementations. |

## Minimum Supported Rust Version (MSRV)

The MSRV is 1.85.0. This will be confirmed once the Turso dependency is added.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.

## Contribution

Contributions are welcome. Please see the [BDK contributing guidelines](https://github.com/bitcoindevkit/bdk/blob/master/CONTRIBUTING.md) and [community standards](https://github.com/bitcoindevkit/bdk/community).
