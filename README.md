<div align="center">

<h1>bdk_turso</h1>

<p>An experimental persistence backend for <code>bdk_wallet</code> that stores wallet state in Turso.</p>

<p>
<a href="https://crates.io/crates/bdk_turso"><img src="https://img.shields.io/crates/v/bdk_turso.svg" alt="Crate Info"></a>
<a href="https://docs.rs/bdk_turso"><img src="https://img.shields.io/badge/docs.rs-bdk_turso-green" alt="Wallet API Docs"></a>
<a href="https://github.com/emma31-dev/bdk_turso/actions?query=workflow:CI"><img src="https://github.com/emma31-dev/bdk_turso/workflows/CI/badge.svg" alt="CI Status"></a>
<a href="#license"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg" alt="MIT or Apache-2.0 Licensed"></a>
</p>

</div>


## What is bdk_turso?

`bdk_turso` is a standalone persistence backend for [`bdk_wallet`](https://docs.rs/bdk_wallet), storing wallet state in [Turso](https://turso.tech/), an in-process, SQLite-compatible database written in Rust. It implements the `AsyncWalletPersister` trait behind the `wallet` feature and `WalletPersister` trait behind `blocking`, without changes to `bdk_wallet` or `bdk_persist`.

`bdk_turso` is forked from [`bdk_sqlite`](https://docs.rs/bdk_sqlite), with its persistence implementation adapted for Turso's async-first API. Its persistence tests are adapted from the test suite in [`bdk_redb`](https://docs.rs/bdk_redb).

### Project status

`bdk_turso` is early-stage and experimental. It currently implements async wallet persistence, and its API may change. `bdk_turso` was built in response to a request for Turso support in bdk_wallet.

### Why Turso?

#### Native async persistence

Turso's async-first design allows `bdk_turso` to implement `AsyncWalletPersister` natively rather than wrapping blocking database operations in `spawn_blocking`. Turso is designed to support concurrent write scaling. Connections wait up to five seconds for a contended write lock before returning an error, matching the default used by SQLite drivers. Reproducible comparisons with `bdk_sqlite` and `bdk_redb` are published in [BENCHMARK.md](BENCHMARK.md); results are workload- and machine-dependent and do not imply that one backend is best for every use case.

#### SQLite compatibility

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
bdk_turso = { version = "0.3", features = ["wallet", "psbt"] }
```

Open a store and call `Store::migrate()` before reading or writing; tables for disabled features are not created. See [Feature flags](#feature-flags) for the full list.

## Feature flags

`Store::migrate()` always creates the core block and transaction tables. Wallet-specific and PSBT tables are created only when their respective features are enabled. The default schema also creates B-tree indexes for `block(hash)`, `txout(script)`, `anchor(txid)`, and `anchor(block_hash)`; the `wallet` schema adds `keychain_script_pubkey(script)`.

| Feature | Description |
| --- | --- |
| `wallet` | Enables wallet-specific tables, the `AsyncWalletPersister` implementation, and (through `blocking`) the blocking `WalletPersister`. |
| `blocking` | Implements the blocking `WalletPersister` for `Store`. Enabled by default by `wallet`. |
| `psbt` | Enables the PSBT table and the `write_psbt`/`read_psbt` storage methods; independent of `wallet`. |
| `remote` | Enables Turso Cloud sync with a local cache. |

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

`Options::new()` leaves every optional setting disabled. Build the options before opening the store and pass them to `Store::new_with_options`, `Store::new_memory_with_options`, or `Store::new_remote_with_options`; the settings apply while Turso opens the database and cannot be changed on an open store.

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

### Transaction behavior

`Options` only exposes settings the crate acts on. Turso engine capabilities that affect only application SQL, such as `ATTACH DATABASE`, `VACUUM`, generated columns, `WITHOUT ROWID`, and alternative index methods, are not exposed. Open your own Turso database or run the corresponding SQL if an application needs them.

`with_transaction_behavior` selects the `BEGIN` mode for every transaction the store opens:

| Behavior | What it changes | When to use it | Engine flags |
| --- | --- | --- | --- |
| `Deferred` (default) | Starts the transaction on first read or write | General use; avoids taking a write lock until a statement needs it | Multi-process WAL for file-backed local databases |
| `Immediate` | Acquires the write lock at `BEGIN` | Writes that should fail fast on contention instead of partway through | Multi-process WAL for file-backed local databases |
| `Exclusive` | Prevents other connections from reading while the transaction is open | Short operations that must block other readers | Multi-process WAL for file-backed local databases |
| `Concurrent` | Allows parallel writers with conflicts detected at commit; switches local databases to MVCC journal mode | Multi-writer workloads on local stores | MVCC passive checkpointing |

The four behaviors are mutually exclusive because Turso stores one behavior per connection. Calling `with_transaction_behavior` again replaces the previous value; the last call wins. `Concurrent` is rejected by remote sync stores, which always use multi-process WAL for the local cache. The engine flags in the last column are enabled automatically; they are not separate builders.

```rust,no_run
use bdk_turso::{Options, Store, TransactionBehavior};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::new()
        .with_transaction_behavior(TransactionBehavior::Immediate);
    let store = Store::new_with_options("wallet.sqlite", options).await?;
    store.migrate().await?;
    Ok(())
}
```

Local at-rest encryption is rejected by remote sync stores with an error, as is `TransactionBehavior::Concurrent`. `with_remote_encryption` is the only encryption option available to them; see [Remote Turso](#remote-turso).

### Remote cache options

With the `remote` feature, `Options` also configures the local sync cache. These builders only apply to remote stores; passing them to `Store::new_with_options` or `Store::new_memory_with_options` returns `Error::UnsupportedOption`.

| Builder | What it changes | When to use it |
| --- | --- | --- |
| `with_bootstrap_if_empty(bool)` | Whether an empty cache downloads the remote schema and data. Defaults to `true`, matching Turso. | Deferred bootstrap, or when the application pre-seeds the cache. |
| `with_long_poll_timeout(Duration)` | How long the sync engine waits for remote changes before returning. | Tuning request frequency against refresh latency. |
| `with_client_name(impl Into<String>)` | The client name reported to the sync engine. Defaults to `turso-sync-rust`. | Stable server-side telemetry and tracing. |
| `with_auth_token_fn(...)` | An async callback that returns a fresh token before every sync request. Overrides the static token passed to the constructor. | Rotating credentials from a secret manager or OAuth flow. |
| `with_partial_sync_opts_experimental(PartialSyncOpts)` | Experimental partial synchronization, which downloads only a segment of the remote database. | Bounding cache size for large remote databases. |
| `with_logical_mvcc_pull(bool)` | Forces logical-MVCC (`true`) or page-stream (`false`) pulls instead of auto-detection. | Tests or unusual deployments; normal operation should leave it unset. |

`PartialSyncOpts` and `PartialBootstrapStrategy` are re-exported from `bdk_turso`.

## Remote Turso

Enable the `remote` feature to connect to Turso Cloud through Turso's local sync engine. The store keeps a local cache at the supplied path, pulls remote changes before reads, and pushes committed writes. An upload can fail after the local commit; the cache retains the change and the persistence operation returns `Error::RemotePush`. Call `store.push().await` to retry uploading pending changes without reapplying the changeset; a failure from this call is also reported as `Error::RemotePush`. The cache is not encrypted by Turso Cloud encryption; configure `Options::with_remote_encryption` for Turso Cloud database encryption and protect the local cache separately.

The cache lifecycle is explicit. `pull()` refreshes the cache and reports whether remote changes were applied, while `push()` uploads local changes; neither does the other. `checkpoint()` folds and reclaims the cache WAL after large changesets, and `flush_cache()` performs `push()` followed by `checkpoint()` so the cache is fully uploaded and compacted. `cache_stats()` returns a crate-owned `CacheStats` with WAL sizes, last pull and push times, the last applied revision, sync network bytes, and change-data-capture operations. `cache_path()` returns the cache file path. For local stores, `pull()` returns `Ok(false)`, `checkpoint()` and `flush_cache()` are no-ops, and `cache_stats()` and `cache_path()` return `None`.

```rust,ignore
use bdk_turso::Store;

let store = Store::new_remote(
    "wallet-cache.db",
    &remote_url,
    &auth_token,
).await?;
store.migrate().await?;

if store.pull().await? {
    // Remote changes were applied to the cache.
}
store.flush_cache().await?;
let stats = store.cache_stats().await?;
```

Local at-rest encryption, MVCC passive checkpointing, and `TransactionBehavior::Concurrent` are not supported by Turso's remote sync builder and return an error if requested. The ignored sync integration test can target a disposable Turso Cloud database or a local Turso sync server. Set `BDK_TURSO_REMOTE_URL` and `BDK_TURSO_AUTH_TOKEN` to the endpoint and token accepted by that server, then run `cargo test --features remote --test persistence remote_store_syncs_changesets_between_local_caches -- --ignored`.

## PSBT persistence

Enable the `psbt` feature to store partially signed transactions. It provides `Store::write_psbt` and `Store::read_psbt` and is independent of the `wallet` feature; `Store::migrate()` creates the `psbt` table only when the feature is enabled.

A PSBT is keyed by the txid of its unsigned transaction, and the complete PSBT is serialized, including signing metadata. Writing another PSBT for the same unsigned transaction replaces the stored one and returns that txid. `Store::read_psbt` returns `Ok(None)` when nothing is stored for the txid.

PSBTs can contain partial signatures, so persistence fails closed on unencrypted databases. Both methods return `Error::PsbtRequiresEncryption` unless the store was opened with `Options::with_encryption`. Stores created without encryption, stores wrapped with `Store::from_database` (whose opening options are unknown), and remote stores (whose local cache is not encrypted) are rejected.

```rust,ignore
use bdk_turso::Store;

let txid = store.write_psbt(&psbt).await?;
let stored = store.read_psbt(txid).await?;
assert_eq!(stored.as_ref(), Some(&psbt));
```

This lets an unsigned PSBT survive a restart, be signed later, and be written back under the same unsigned txid. The `transaction` example builds, reloads, verifies, signs, and broadcasts a PSBT this way; see [Examples](#examples). PSBT writes follow the [persistence conflict behavior](#persistence-conflict-behavior) described below.

## Persistence conflict behavior

`bdk_turso` uses last-write-wins behavior for keyed persistence writes: when an incoming write targets an existing key, its value replaces the stored value rather than being ignored. This applies to component and wallet changeset writes, including a replacement block hash at the same height, and to PSBT writes. Explicit deletion entries remove their rows. This is storage-level behavior, not semantic validation of which value is newer or correct; ordering between independent remote clients follows Turso's sync behavior.

## Benchmarks

The repository includes a reproducible benchmark suite in [`benches/backends.rs`](benches/backends.rs) that runs the same generated wallet changesets through `bdk_turso`, `bdk_sqlite`, and `bdk_redb`. It measures changeset read and write speed, concurrent writes at several writer counts, opening an existing database, and on-disk storage size. Run it with:

```sh
cargo bench --bench backends
```

Sample counts, batch sizes, writer counts, dataset shape, Turso options, and the included backends can be set in an optional `benches/config.toml`; copy `benches/example.toml` to that path and edit it, or pass `-- --config path/to/config.toml`. Without a config file the defaults are used.

Detailed results, environment details, methodology, and limitations are in [BENCHMARK.md](BENCHMARK.md). Results are kept for each measured release and are not CI performance thresholds.

## Examples

Both examples read `examples/config.toml`. Copy the committed template and edit the local copy:

```sh
cp examples/example.toml examples/config.toml
```

`examples/config.toml` is ignored by git, so credentials stay out of the repository. Set the database path, network, Bitcoin Core RPC URL and credentials, external and internal descriptors, and optionally a recipient address. Add `encryption_hexkey` (64 hexadecimal characters) to open the database with local at-rest encryption; it is required by the transaction example because PSBT persistence is rejected on unencrypted databases. The template ships regtest-only sample keys; replace them before using another network. A database created without encryption cannot later be opened with a key. If the file is missing or invalid, the examples stop with an error explaining how to create it and which value is wrong.

- `cargo run --example wallet` loads the config, creates or loads the wallet, reveals a receive address, and syncs with a local Bitcoin Core regtest node.
- `cargo run --features psbt --example transaction` syncs a regtest wallet, stores and reloads an unsigned PSBT, confirms it round-trips, signs it, broadcasts it, and persists the resulting wallet changeset. It requires `encryption_hexkey`. `recipient_address` is optional until this example reaches the send step; if the wallet has no spendable funds, it prints a receive address — fund it, mine 101 blocks, then rerun.

## Minimum Supported Rust Version (MSRV)

The MSRV is 1.88.0, based on the current resolved dependency graph.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option.

## Contribution

Contributions are welcome. Please read the [contribution guide](CONTRIBUTING.md), the [BDK contributing guidelines](https://github.com/bitcoindevkit/bdk/blob/master/CONTRIBUTING.md), and the [BDK community standards](https://github.com/bitcoindevkit/bdk/community). Maintainers should follow [RELEASING.md](RELEASING.md) when publishing a release.

## Disclaimer and attribution

`bdk_turso` is experimental software provided under the [MIT](LICENSE-MIT) or [Apache License, Version 2.0](LICENSE-APACHE), at your option. It is provided "as is", without warranty of any kind, and you use it at your own risk. The license texts contain the full warranty disclaimer and limitation of liability; this section does not replace, modify, or expand them.

**Back up your data.** Manually changing database tables, rows, WAL files, or sync-cache files outside the APIs documented here can corrupt the database or cause irreversible wallet data loss. Keep backups, test recovery, and avoid editing the database directly unless you understand the consequences.

**Attribution and modified forks.** `bdk_turso` is forked from [`bdk_sqlite`](https://docs.rs/bdk_sqlite), with persistence tests adapted from [`bdk_redb`](https://docs.rs/bdk_redb). Copyright and license notices are in `LICENSE-MIT` and `LICENSE-APACHE`. If you redistribute this project or a modified version, keep those notices, identify the changes you made, and do not imply that the original authors or the BDK project endorse your fork. Mark modified builds as unofficial or use a distinct name so users do not confuse them with this project. These are project expectations, not additional license terms; nothing here restricts uses permitted by the licenses or grants trademark rights.

The project does not add use restrictions beyond the licenses. This section is project documentation, not legal advice. If you redistribute or modify `bdk_turso`, review the license and any applicable attribution requirements with qualified counsel.
