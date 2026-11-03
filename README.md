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

`bdk_turso` stays within Turso's SQLite-compatible feature subset and avoids Turso-only extensions. Unencrypted database files can be read by `bdk_sqlite`; Turso-encrypted files require Turso to open. Do not call `bdk_sqlite::Store::migrate()` on a `bdk_turso` database: it has no SQLx migration history, so `bdk_sqlite` runs migrations containing `DROP TABLE IF EXISTS` statements and erases the existing wallet state.

## Installation

`bdk_turso` requires Rust 1.88.0 or newer and an async runtime; the examples and persistence calls use `tokio`. The `turso` dependency is compiled from source as a pure-Rust crate, so no separate database server is needed.

Add the crate with the features you need:

```sh
cargo add bdk_turso                 # wallet persistence, the default feature
cargo add bdk_turso --features psbt # plus PSBT storage
cargo add bdk_turso --features remote
```

Or declare it directly in `Cargo.toml`:

```toml
[dependencies]
bdk_turso = { version = "0.2", features = ["wallet", "psbt"] }
```

Open a store and call `Store::migrate()` before reading or writing; tables for disabled features are not created. See [Feature flags](#feature-flags) for the full list.

## Usage

With the `wallet` feature enabled, `Store` implements BDK's async wallet persistence API. Replace the descriptor strings below with valid descriptors for your wallet.

```rust,no_run
use bdk_turso::Store;
use bdk_wallet::{bitcoin::Network, KeychainKind, Wallet};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let external_descriptor = "wpkh(...)";
    let internal_descriptor = "wpkh(...)";
    let mut store = Store::new("wallet.sqlite").await?;
    store.migrate().await?;

    let mut wallet = match Wallet::load()
        .descriptor(KeychainKind::External, Some(external_descriptor))
        .descriptor(KeychainKind::Internal, Some(internal_descriptor))
        .check_network(Network::Regtest)
        .load_wallet_async(&mut store)
        .await?
    {
        Some(wallet) => wallet,
        None => Wallet::create(external_descriptor, internal_descriptor)
            .network(Network::Regtest)
            .create_wallet_async(&mut store)
            .await?,
    };

    let address = wallet.reveal_next_address(KeychainKind::External);
    wallet.persist_async(&mut store).await?;
    println!("Receive address: {}", address.address);
    Ok(())
}
```

### Blocking persistence

`wallet` enables the `blocking` feature by default, so `Store` also implements BDK's blocking `WalletPersister` and the non-async `load_wallet` and `create_wallet` APIs work too:

```rust,no_run
use bdk_turso::Store;
use bdk_wallet::{bitcoin::Network, KeychainKind, Wallet};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = Store::new("wallet.sqlite").await?;
    let _wallet = Wallet::load()
        .descriptor(KeychainKind::External, Some("wpkh(...)"))
        .check_network(Network::Regtest)
        .load_wallet(&mut store)?;
    Ok(())
}
```

Each blocking call runs on a dedicated thread with its own Tokio runtime, so it is safe to call from inside an async runtime. It still blocks the calling thread until the operation finishes, so prefer the async persister on async hot paths.

## Options and encryption

`Options::new()` leaves all experimental Turso settings disabled. Build the options before opening the store and pass them to `Store::new_with_options`, `Store::new_memory_with_options`, or `Store::new_remote_with_options`; the settings apply while Turso opens the database and cannot be changed on an open store.

### Encryption

Local at-rest encryption is the main option. It is configured with the publicly re-exported `EncryptionOpts` and `Options::with_encryption`; no separate Cargo feature is required. Supply a 32-byte key as hexadecimal through a secret manager or environment variable, never a committed literal. Keep the same cipher and key to reopen the database; losing the key makes its data inaccessible.

`generate_encryption_opts()` builds options for a new database: it sets `cipher` to `aes256gcm` and fills `hexkey` with a random 32-byte key. Store that hex key safely before opening the database and reuse it to reopen it.

```rust,no_run
use bdk_turso::{EncryptionOpts, Options, Store};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::new().with_encryption(EncryptionOpts {
        cipher: "aes256gcm".to_owned(),
        hexkey: std::env::var("TURSO_ENCRYPTION_HEXKEY")?,
    });
    let store = Store::new_with_options("wallet-encrypted.sqlite", options).await?;
    store.migrate().await?;
    Ok(())
}
```

Local encryption is not supported for remote sync stores, and Turso Cloud encryption does not encrypt the local cache; see [Remote Turso](#remote-turso).

### Other Turso options

The remaining builders gate Turso capabilities that `bdk_turso` does not need for its own schema. Enable only the ones your application uses.

| Builder | Effect |
| --- | --- |
| `with_index_method` | Enables Turso's experimental index-method support. |
| `with_multiprocess_wal` | Allows the same database file to be shared between operating-system processes through Turso's WAL coordinator. |
| `with_vacuum` | Makes `VACUUM` available; it does not run vacuuming automatically. |
| `with_attach` | Enables `ATTACH DATABASE` and `DETACH DATABASE`. |
| `with_generated_columns` | Enables generated columns in application SQL; it does not alter the wallet schema. |
| `with_without_rowid` | Enables `WITHOUT ROWID` table definitions. |
| `with_mvcc_passive_checkpoint` | Enables Turso's experimental passive-checkpoint behavior for MVCC databases; it does not start a checkpoint. |

```rust,no_run
use bdk_turso::{Options, Store};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::new().with_attach().with_generated_columns();
    let store = Store::new_with_options("wallet.sqlite", options).await?;
    store.migrate().await?;
    Ok(())
}
```

Local at-rest encryption and `with_mvcc_passive_checkpoint` are rejected by remote sync stores with an error. `with_remote_encryption` is the only encryption option available to them; see [Remote Turso](#remote-turso).

## PSBT persistence

Enable the `psbt` feature to store partially signed transactions. It provides `Store::write_psbt` and `Store::read_psbt` and is independent of the `wallet` feature; `Store::migrate()` creates the `psbt` table only when the feature is enabled.

A PSBT is keyed by the txid of its unsigned transaction, and the complete PSBT is serialized, including signing metadata. Writing another PSBT for the same unsigned transaction replaces the stored one and returns that txid. `Store::read_psbt` returns `Ok(None)` when nothing is stored for the txid.

```rust,ignore
use bdk_turso::Store;

let txid = store.write_psbt(&psbt).await?;
let stored = store.read_psbt(txid).await?;
assert_eq!(stored.as_ref(), Some(&psbt));
```

This lets an unsigned PSBT survive a restart, be signed later, and be written back under the same unsigned txid. The `transaction` example builds, reloads, verifies, signs, and broadcasts a PSBT this way; see [Examples](#examples). PSBT writes follow the [persistence conflict behavior](#persistence-conflict-behavior) described below.

## Persistence conflict behavior

`bdk_turso` uses last-write-wins behavior for keyed persistence writes: when an incoming write targets an existing key, its value replaces the stored value rather than being ignored. This applies to component and wallet changeset writes, including a replacement block hash at the same height, and to PSBT writes. Explicit deletion entries remove their rows. This is storage-level behavior, not semantic validation of which value is newer or correct; ordering between independent remote clients follows Turso's sync behavior.

## Examples

- `cargo run --example wallet` syncs a wallet with a local Bitcoin Core regtest node.
- `cargo run --features psbt --example transaction` syncs a regtest wallet, stores and reloads an unsigned PSBT, confirms it matches, signs and broadcasts it, and persists the resulting wallet changeset. It requires `POLAR_RPC_USER`, `POLAR_RPC_PASS`, and a regtest `RECIPIENT_ADDRESS`. If the wallet has no spendable funds, it prints a receive address; fund it and mine 101 blocks before rerunning.

Both examples requires Polar.

## Remote Turso

Enable the `remote` feature to connect to Turso Cloud through Turso's local sync engine. The store keeps a local cache at the supplied path, pulls remote changes before reads, and pushes committed writes. An upload can fail after the local commit; the cache retains the change and the persistence operation returns `Error::RemotePush`. Call `store.push().await` to retry uploading pending changes without reapplying the changeset; a failure from this call is also reported as `Error::RemotePush`. The cache is not encrypted by Turso Cloud encryption; configure `Options::with_remote_encryption` for Turso Cloud database encryption and protect the local cache separately.

```rust,ignore
use bdk_turso::Store;

let store = Store::new_remote(
    "wallet-cache.db",
    &remote_url,
    &auth_token,
).await?;
store.migrate().await?;
```

Local at-rest encryption and MVCC passive checkpointing are not supported by Turso's remote sync builder and return an error if requested. The ignored sync integration test can target a disposable Turso Cloud database or a local Turso sync server. Set `BDK_TURSO_REMOTE_URL` and `BDK_TURSO_AUTH_TOKEN` to the endpoint and token accepted by that server, then run `cargo test --features remote --test persistence remote_store_syncs_changesets_between_local_caches -- --ignored`.

## Feature flags

`Store::migrate()` always creates the core block and transaction tables. Wallet-specific and PSBT tables are created only when their respective features are enabled.

| Feature | Description |
| --- | --- |
| `wallet` | Enables wallet-specific tables, the `AsyncWalletPersister` implementation, and (through `blocking`) the blocking `WalletPersister`. |
| `blocking` | Implements the blocking `WalletPersister` for `Store`. Enabled by default by `wallet`. |
| `psbt` | Enables the PSBT table and the `write_psbt`/`read_psbt` storage methods; independent of `wallet`. |
| `remote` | Enables Turso Cloud sync with a local cache. |

## Minimum Supported Rust Version (MSRV)

The MSRV is 1.88.0, based on the current resolved dependency graph.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.

## Contribution

Contributions are welcome. Please read the [contribution guide](CONTRIBUTING.md), the [BDK contributing guidelines](https://github.com/bitcoindevkit/bdk/blob/master/CONTRIBUTING.md), and the [BDK community standards](https://github.com/bitcoindevkit/bdk/community).
