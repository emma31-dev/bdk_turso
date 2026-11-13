# Benchmarks

This document publishes reproducible persistence benchmarks for `bdk_turso` and two alternative BDK backends: [`bdk_sqlite`](https://crates.io/crates/bdk_sqlite) and [`bdk_redb`](https://crates.io/crates/bdk_redb). The goal is transparent comparison, not a claim that one backend is universally better. Results depend on the workload, hardware, filesystem, and dependency versions.

The suite lives in [`benches/backends.rs`](benches/backends.rs). It runs the same generated wallet changesets through every backend and does not use network-dependent workloads. It is not run in CI and has no performance thresholds.

## Running the suite

```sh
# Full run (30 samples per workload, 5 warmup samples)
cargo bench --bench backends

# Fewer samples for a quick check
cargo bench --bench backends -- --quick

# Load custom settings from a file
cargo bench --bench backends -- --config path/to/config.toml

# Also write the generated Markdown report to a file
cargo bench --bench backends -- --output target/bench-report.md
```

The report printed by the suite includes the environment, effective configuration, workload definition, timings, and storage measurements. To publish a new result, run the suite on a quiet machine and add the report under [Results history](#results-history) without removing earlier runs. Do not estimate or reconstruct results that were never measured.

## Benchmark configuration

Settings optionally come from `benches/config.toml`. Copy the committed template and edit the local copy; the local file is ignored by git:

```sh
cp benches/example.toml benches/config.toml
```

If the file is absent, the built-in defaults are used, so `cargo bench --bench backends` works without any setup. `--config path` loads a different file and fails if it does not exist or is invalid. `--quick` overrides the sample and batch settings but keeps the configured writers, dataset, Turso options, and backends.

| Setting | Default | Meaning |
| --- | --- | --- |
| `samples` | `30` | Measured samples per workload. |
| `warmup_samples` | `5` | Unmeasured samples before each workload. |
| `write_batch` | `10` | Sequential writes per sample. |
| `read_batch` | `50` | Sequential reads per sample. |
| `open_batch` | `3` | Database opens per sample. |
| `concurrent_rounds` | `5` | Concurrent-write rounds per sample. |
| `writers` | `[1, 2, 4, 8]` | Writer counts for the concurrent-write workload. The largest value sizes the Tokio worker pool. |
| `storage_changesets` | `64` | Distinct changesets persisted for the storage measurement. |
| `dataset.transactions` | `16` | Transactions per generated changeset. |
| `dataset.blocks` | `32` | Local-chain entries per changeset. |
| `dataset.scripts_per_transaction` | `8` | Cached scripts per descriptor per transaction. |
| `dataset.locked_outpoints` | `8` | Locked outpoints per changeset. |
| `turso.multiprocess_wal` | `true` | Open Turso with its opt-in multi-process WAL mode. |
| `backends.turso`, `backends.sqlite`, `backends.redb` | `true` | Backends included in the run; at least one must be enabled. |

All settings are optional and unknown keys are rejected. The suite fails with an actionable error when a value is zero, `writers` is empty or contains zero, or no backend is enabled.

## Methodology

- Every backend receives the same generated `bdk_wallet::ChangeSet` values. By default, each changeset contains 16 transactions, 32 local-chain entries, 128 cached scripts per descriptor, 8 locked outpoints, both descriptors, and the network.
- Databases are file-backed in a temporary directory, so measurements include persistence and filesystem behavior. The same workload runs on each backend in the same process, one backend at a time.
- The async backends (`bdk_turso` and `bdk_sqlite`) are driven by a Tokio multi-thread runtime whose worker pool is sized to the largest configured writer count (8 by default). `bdk_redb` is synchronous and concurrent writers use operating-system threads.
- `bdk_turso` is opened with `Options::new().with_multiprocess_wal()`, its opt-in multi-process WAL mode, and each concurrent writer opens its own database handle on the same file. All other experimental Turso options are disabled. This is controlled by `turso.multiprocess_wal` in the configuration. `bdk_sqlite` and `bdk_redb` use their default constructors.
- Changeset write and read timings reuse one changeset after a warmup phase. The database therefore reaches a steady size and later samples measure repeated updates rather than unbounded growth.
- Concurrent writes start all writers from a barrier and give each writer a distinct changeset. Reported latency is per write; throughput is the number of writers divided by the median round time.
- `bdk_turso` connections wait up to five seconds for a contended write lock before returning an error. `bdk_sqlite` uses its SQLx busy timeout, also five seconds by default. `bdk_redb` blocks when another write transaction is active.
- Database-opening timings open an existing database that already contains the schema and seed data. They measure each backend's open path rather than schema migration.
- Storage sizes are measured after persisting the configured number of distinct changesets (64 by default) and closing the store. The value is the total size of every file belonging to that database. For SQLite-compatible databases this includes `-wal` and `-shm` sidecars; no checkpoint, truncation, or vacuum is performed.
- The default full run collects 30 samples per workload after 5 warmup samples. Reads use batches of 50 operations, writes batches of 10, concurrent measurements 5 rounds per sample, and opening batches of 3. These values are configurable; the median and 95th percentile are always reported.

## Limitations

- The backends expose different APIs. `bdk_turso` and `bdk_sqlite` persist a complete wallet changeset in one transaction. `bdk_redb::Store::persist_wallet` performs several write transactions (network, keychains, local chain, indexer, transaction graph, and locked outpoints). The timings compare complete wallet persistence, not identical transaction boundaries or isolation behavior.
- `bdk_redb` is synchronous, while the other backends are async. The benchmark drives async backends with a Tokio runtime. Runtime scheduling and fsync behavior can differ from an application's configuration.
- `bdk_turso` uses Turso's SQLite-compatible engine; `bdk_sqlite` uses SQLite through `sqlx`. Even with the same logical rows, page layout, journaling, and file sizes differ, so storage sizes are only broadly indicative of a backend's footprint for this dataset.
- The `remote` feature is not benchmarked because it depends on a network service. Local at-rest encryption is not benchmarked because it is not supported by the alternative backends and would make the workloads incomparable.
- All backends are exercised from a single process. `bdk_turso` uses its opt-in multi-process WAL mode even though the benchmark does not share the database between operating-system processes. Results with that option disabled may differ, especially under concurrent writes.
- Concurrent-write behavior at the database level is not a transaction-isolation or durability audit. Backends may serialize writes internally, retry, or reject a write under contention depending on their configuration.
- File-system cache state, storage hardware, CPU frequency scaling, and background load affect results. Numbers from one machine should not be treated as predictions for another.

## Results history

Results are grouped by `bdk_turso` version and the environment in which they were measured. Keep earlier subsections unchanged when adding a new release.

Releases before 0.3.0 were not measured with this suite. No historical figures are published for them rather than estimating values that were never recorded.

### Unreleased (after 0.3.0)

Measured at merge commit `291d6331bfb5bb5b66fb7e1e79d2d3732ba04c83`, which includes the default lookup indexes and transaction-behavior options from #22, #23, and #24. The working tree was dirty because of untracked local files; no tracked files differed from the commit.

This run used `--quick`: 5 samples, 1 warmup sample, batches of 5 writes and 10 reads, 2 concurrent rounds, and 1 open per sample. Writer counts, dataset shape, storage changesets, and Turso options are the defaults.

Generated on 2026-11-13T15:21:24Z with 5 samples and 1 warmup sample (quick mode).

#### Environment

| Item | Value |
| --- | --- |
| Architecture | x86_64 |
| Async runtime | Tokio multi-thread, 8 worker threads |
| CPU | Intel(R) Core(TM) i5-5350U CPU @ 1.80GHz |
| Cargo | cargo 1.97.1 (c980f4866 2026-06-30) |
| Crate features | wallet=true, psbt=false, remote=false, blocking=true |
| Git commit | 291d6331bfb5bb5b66fb7e1e79d2d3732ba04c83 |
| Git status | dirty |
| Memory | 8.0 GiB |
| Operating system | Darwin 21.6.0 x86_64 |
| Profile | cargo bench (debug assertions: false) |
| Rust | rustc 1.97.1 (8bab26f4f 2026-07-14) |
| bdk_chain | 0.23.4 |
| bdk_redb | 0.2.0 |
| bdk_sqlite | 0.6.0 |
| bdk_turso | 0.3.0 |
| bdk_turso options | multiprocess WAL enabled; other experimental options disabled |
| bdk_wallet | 3.2.0 |
| redb (bdk_redb) | 2.6.3 |
| sqlx (bdk_sqlite) | 0.8.6 |
| turso | 0.8.3 |

#### Workload timings

Rows are grouped by workload and ordered fastest to slowest so the backends can be compared side by side. Only the 8-writer concurrent case is shown; the suite also measures 1, 2, and 4 writers.

In this quick run, `bdk_sqlite` was fastest for writes, 8-writer concurrent throughput, database opens, and on-disk storage. `bdk_redb` was fastest for reads but slowest for writes and opens. `bdk_turso` was second for writes, reads, and opens, but slowest at 8 concurrent writers. Quick mode uses few samples, so treat these rankings as indicative rather than definitive.

| Workload | Backend | Writers | Median | p95 | Throughput | Notes |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| write changeset | bdk_sqlite | - | 11.76 ms | 12.28 ms | 85.0/s | Fastest |
| write changeset | bdk_turso | - | 37.06 ms | 40.09 ms | 27.0/s | Second |
| write changeset | bdk_redb | - | 118.00 ms | 130.66 ms | 8.5/s | Slowest |
| read changeset | bdk_redb | - | 457.11 µs | 558.12 µs | 2.2k/s | Fastest |
| read changeset | bdk_turso | - | 2.03 ms | 2.57 ms | 493.3/s | Second |
| read changeset | bdk_sqlite | - | 3.99 ms | 5.33 ms | 250.8/s | Slowest |
| concurrent write | bdk_sqlite | 8 | 33.37 ms | 35.09 ms | 30.0/s | Fastest |
| concurrent write | bdk_redb | 8 | 82.49 ms | 93.35 ms | 12.1/s | Second |
| concurrent write | bdk_turso | 8 | 154.61 ms | 206.41 ms | 6.5/s | Slowest |
| open existing database | bdk_sqlite | - | 406.76 µs | 541.72 µs | 2.5k/s | Fastest |
| open existing database | bdk_turso | - | 1.66 ms | 2.57 ms | 603.9/s | Second |
| open existing database | bdk_redb | - | 44.39 ms | 45.83 ms | 22.5/s | Slowest |

#### On-disk storage

Total bytes of all files belonging to the seeded database, measured immediately after the final commit. SQLite-compatible files include their `-wal` and `-shm` sidecars; no checkpoint or vacuum is performed.

Dataset: 64 distinct wallet changesets (16 transactions each).

| Backend | Bytes | MiB | Notes |
| --- | ---: | ---: | --- |
| bdk_sqlite | 1253376 | 1.20 | Smallest |
| bdk_turso | 2998416 | 2.86 | Second |
| bdk_redb | 4755456 | 4.54 | Largest |

#### Workload definition

- bdk_turso is opened with `Options::new().with_multiprocess_wal()`; all other experimental options are disabled. Each Turso concurrent writer opens its own database handle on the same file. The other backends use their default constructors.
- Each changeset contains 16 transactions, 32 local-chain entries, 128 cached scripts per descriptor, and 8 locked outpoints, plus both descriptors and the network.
- Write and read measurements reuse the same changeset, so the database stops growing after the warmup phase and later samples measure steady-state updates.
- Concurrent write rounds start all writers from a barrier and persist one distinct changeset per writer. Reported latency is per write; throughput is writers divided by the median round time.
- Open measurements open an existing database that already contains the schema and seed data. They measure the backend's open path, not schema migration.

<!-- Add a new subsection for each measured release. Keep earlier subsections unchanged. -->
