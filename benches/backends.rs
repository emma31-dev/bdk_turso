//! Reproducible wallet-persistence benchmarks for `bdk_turso`, `bdk_sqlite`, and `bdk_redb`.
//!
//! Every backend receives the same generated wallet changesets and runs the same workload:
//!
//! - Persisting a complete wallet changeset.
//! - Reading a complete wallet changeset.
//! - Concurrent changeset writes at 1, 2, 4, and 8 writers.
//! - Opening a database whose schema and seed data already exist.
//! - On-disk storage size after persisting the same history.
//!
//! Run the suite with:
//!
//! ```sh
//! cargo bench --bench backends
//! ```
//!
//! Add `-- --quick` for fewer samples, or `-- --output path/to/report.md` to also write the
//! Markdown report to a file. Results are machine dependent; they are not CI thresholds.
//!
//! Settings are read from `benches/config.toml` when it exists. Copy `benches/example.toml` to
//! that path to customize the run; `benches/config.toml` is ignored by git. Pass
//! `-- --config path/to/config.toml` to load a different file.

use std::{
    collections::BTreeMap,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

use bdk_chain::{
    BlockId, ConfirmationBlockTime, DescriptorExt, DescriptorId,
    bitcoin::{
        Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid, absolute,
        hashes::Hash, transaction,
    },
    keychain_txout, local_chain,
    miniscript::descriptor::{Descriptor, DescriptorPublicKey},
    tx_graph,
};
use bdk_redb::{Store as RedbStore, redb::Database as RedbDatabase};
use bdk_sqlite::Store as SqliteStore;
use bdk_turso::Store as TursoStore;
use bdk_wallet::{ChangeSet, locked_outpoints};
use tempfile::{TempDir, tempdir};
use tokio::runtime::Runtime;

/// Descriptor from the persistence tests. It controls no real funds.
const EXTERNAL_DESCRIPTOR: &str = "wpkh([41f2aed0/84h/1h/0h]tpubDDFSdQWw75hk1ewbwnNpPp5DvXFRKt68ioPoyJDY752cNHKkFxPWqkqCyCf4hxrEfpuxh46QisehL3m8Bi6MsAv394QVLopwbtfvryFQNUH/0/*)#g0w0ymmw";
/// Descriptor from the persistence tests. It controls no real funds.
const INTERNAL_DESCRIPTOR: &str = "wpkh([41f2aed0/84h/1h/0h]tpubDDFSdQWw75hk1ewbwnNpPp5DvXFRKt68ioPoyJDY752cNHKkFxPWqkqCyCf4hxrEfpuxh46QisehL3m8Bi6MsAv394QVLopwbtfvryFQNUH/1/*)#emtwewtk";

/// Wallet name used for the redb wallet tables.
const REDB_WALLET_NAME: &str = "benchmark";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|arg| arg == "--bench") {
        // `cargo test --all-targets` executes bench binaries. Do not run measurements there.
        eprintln!("backends benchmark: run with `cargo bench --bench backends`");
        return;
    }

    if let Err(error) = run(&args) {
        eprintln!("benchmark failed: {error:#}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let config = Config::load(args)?;
    let output_path = args
        .iter()
        .position(|arg| arg == "--output")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);

    if !config.backends.any() {
        bail!("no backend is enabled in the benchmark config");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.max_writers())
        .enable_all()
        .build()?;
    let directory = tempdir()?;
    let dataset = Dataset::new(&config);
    let mut results = Vec::new();
    let mut storage = Vec::new();

    eprintln!("benchmarking {}", config.backends.list());

    if config.backends.turso {
        benchmark_turso(
            &runtime,
            &directory,
            &dataset,
            &config,
            &mut results,
            &mut storage,
        )?;
    }
    if config.backends.sqlite {
        benchmark_sqlite(
            &runtime,
            &directory,
            &dataset,
            &config,
            &mut results,
            &mut storage,
        )?;
    }
    if config.backends.redb {
        benchmark_redb(&directory, &dataset, &config, &mut results, &mut storage)?;
    }

    let report = render_report(&config, &results, &storage);
    print!("{report}");
    if let Some(path) = output_path {
        fs::write(&path, &report)?;
        eprintln!("wrote report to {}", path.display());
    }

    Ok(())
}

/// Benchmark settings loaded from `benches/config.toml`.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    /// Use the short preset; set by `--quick`, never read from the file.
    #[serde(skip)]
    quick: bool,
    /// Measured samples per workload.
    samples: usize,
    /// Unmeasured warmup samples before each workload.
    warmup_samples: usize,
    /// Sequential writes per sample.
    write_batch: usize,
    /// Sequential reads per sample.
    read_batch: usize,
    /// Rounds per concurrent-write sample.
    concurrent_rounds: usize,
    /// Database opens per sample.
    open_batch: usize,
    /// Writer counts measured by the concurrent-write workload.
    writers: Vec<usize>,
    /// Distinct changesets persisted when measuring on-disk storage.
    storage_changesets: u64,
    /// Shape of each generated wallet changeset.
    dataset: DatasetConfig,
    /// Turso-specific benchmark options.
    turso: TursoConfig,
    /// Backends included in the run.
    backends: BackendSelection,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quick: false,
            samples: 30,
            warmup_samples: 5,
            write_batch: 10,
            read_batch: 50,
            concurrent_rounds: 5,
            open_batch: 3,
            writers: vec![1, 2, 4, 8],
            storage_changesets: 64,
            dataset: DatasetConfig::default(),
            turso: TursoConfig::default(),
            backends: BackendSelection::default(),
        }
    }
}

impl Config {
    fn load(args: &[String]) -> anyhow::Result<Self> {
        let explicit_path = args.iter().position(|arg| arg == "--config");
        let mut config = if let Some(index) = explicit_path {
            let path = args.get(index + 1).context("`--config` requires a path")?;
            Self::from_file(Path::new(path))?
        } else {
            let path = Self::default_path();
            if path.exists() {
                Self::from_file(&path)?
            } else {
                Self::default()
            }
        };

        if args.iter().any(|arg| arg == "--quick") {
            config.apply_quick_preset();
        }
        config.validate()?;
        Ok(config)
    }

    /// Path of the optional local benchmark configuration.
    fn default_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/config.toml")
    }

    fn from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read benchmark config {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("invalid TOML benchmark config at {}", path.display()))
    }

    fn apply_quick_preset(&mut self) {
        self.quick = true;
        self.samples = 5;
        self.warmup_samples = 1;
        self.write_batch = 5;
        self.read_batch = 10;
        self.concurrent_rounds = 2;
        self.open_batch = 1;
    }

    fn validate(&self) -> anyhow::Result<()> {
        for (name, value) in [
            ("samples", self.samples),
            ("write_batch", self.write_batch),
            ("read_batch", self.read_batch),
            ("concurrent_rounds", self.concurrent_rounds),
            ("open_batch", self.open_batch),
        ] {
            if value == 0 {
                bail!("`{name}` must be greater than zero in the benchmark config");
            }
        }
        if self.storage_changesets == 0 {
            bail!("`storage_changesets` must be greater than zero in the benchmark config");
        }
        if self.writers.is_empty() {
            bail!("`writers` must contain at least one writer count in the benchmark config");
        }
        if self.writers.contains(&0) {
            bail!("`writers` must not contain zero in the benchmark config");
        }
        self.dataset.validate()?;
        Ok(())
    }

    /// Largest configured writer count, used for the runtime worker pool and writer dataset.
    fn max_writers(&self) -> usize {
        self.writers.iter().copied().max().unwrap_or(1)
    }
}

/// Shape of every generated wallet changeset.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DatasetConfig {
    /// Transactions per changeset.
    transactions: u32,
    /// Local-chain entries per changeset.
    blocks: u32,
    /// Cached scripts per descriptor per transaction.
    scripts_per_transaction: u32,
    /// Locked outpoints per changeset.
    locked_outpoints: u32,
}

impl Default for DatasetConfig {
    fn default() -> Self {
        Self {
            transactions: 16,
            blocks: 32,
            scripts_per_transaction: 8,
            locked_outpoints: 8,
        }
    }
}

impl DatasetConfig {
    fn validate(&self) -> anyhow::Result<()> {
        for (name, value) in [
            ("dataset.transactions", self.transactions),
            ("dataset.blocks", self.blocks),
            (
                "dataset.scripts_per_transaction",
                self.scripts_per_transaction,
            ),
        ] {
            if value == 0 {
                bail!("`{name}` must be greater than zero in the benchmark config");
            }
        }
        Ok(())
    }
}

/// Turso-specific benchmark options.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TursoConfig {
    /// Open Turso with its opt-in multi-process WAL mode.
    multiprocess_wal: bool,
}

impl Default for TursoConfig {
    fn default() -> Self {
        Self {
            multiprocess_wal: true,
        }
    }
}

/// Backends included in the run.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BackendSelection {
    turso: bool,
    sqlite: bool,
    redb: bool,
}

impl Default for BackendSelection {
    fn default() -> Self {
        Self {
            turso: true,
            sqlite: true,
            redb: true,
        }
    }
}

impl BackendSelection {
    fn any(&self) -> bool {
        self.turso || self.sqlite || self.redb
    }

    fn list(&self) -> String {
        let mut enabled = Vec::new();
        if self.turso {
            enabled.push("bdk_turso");
        }
        if self.sqlite {
            enabled.push("bdk_sqlite");
        }
        if self.redb {
            enabled.push("bdk_redb");
        }
        enabled.join(", ")
    }
}

/// Generated inputs shared by every backend.
struct Dataset {
    /// Representative changeset used for write, read, and concurrency measurements.
    changeset: Arc<ChangeSet>,
    /// One changeset per possible writer, so concurrent writers update distinct rows.
    writer_changesets: Vec<Arc<ChangeSet>>,
    /// Distinct changesets persisted to compare on-disk storage sizes.
    history: Vec<ChangeSet>,
}

impl Dataset {
    fn new(config: &Config) -> Self {
        let changeset = Arc::new(full_changeset(1, &config.dataset));
        let writer_changesets = (0..config.max_writers() as u64)
            .map(|writer| Arc::new(full_changeset(100 + writer, &config.dataset)))
            .collect();
        let history = (0..config.storage_changesets)
            .map(|seed| full_changeset(1_000 + seed, &config.dataset))
            .collect();
        Self {
            changeset,
            writer_changesets,
            history,
        }
    }
}

/// Turso options used by the benchmark.
///
/// Multi-process WAL is opt-in in `bdk_turso`; enabling it lets multiple connections take part in
/// the concurrent-write workload. All other experimental options stay disabled.
fn turso_options(config: &Config) -> bdk_turso::Options {
    let options = bdk_turso::Options::new();
    if config.turso.multiprocess_wal {
        options.with_multiprocess_wal()
    } else {
        options
    }
}

fn benchmark_turso(
    runtime: &Runtime,
    directory: &TempDir,
    dataset: &Dataset,
    config: &Config,
    results: &mut Vec<Measurement>,
    storage: &mut Vec<StorageMeasurement>,
) -> anyhow::Result<()> {
    let work_path = directory.path().join("work-turso.db");
    let open_path = directory.path().join("open-turso.db");
    let size_path = directory.path().join("size-turso.db");
    let work_path = path_str(&work_path);
    let open_path = path_str(&open_path);
    let size_path = path_str(&size_path);

    let store = runtime.block_on(async {
        let store = TursoStore::new_with_options(work_path, turso_options(config)).await?;
        store.migrate().await?;
        store.write_changeset(&dataset.changeset).await?;
        anyhow::Ok(store)
    })?;

    let changeset = Arc::clone(&dataset.changeset);
    let write = measure(config, config.write_batch, |operations| {
        runtime.block_on(async {
            for _ in 0..operations {
                store.write_changeset(black_box(&changeset)).await.unwrap();
            }
        });
    });
    results.push(Measurement::sequential(
        "write changeset",
        "bdk_turso",
        write,
    ));

    let read = measure(config, config.read_batch, |operations| {
        runtime.block_on(async {
            for _ in 0..operations {
                let loaded = store.read_changeset().await.unwrap();
                black_box(loaded);
            }
        });
    });
    results.push(Measurement::sequential("read changeset", "bdk_turso", read));
    drop(store);

    // Each writer opens its own database handle. Multi-process WAL coordinates handles on the
    // same file, which is the setting this option exists for.
    for &writers in &config.writers {
        let stores = runtime.block_on(async {
            let mut stores = Vec::with_capacity(writers);
            for _ in 0..writers {
                stores.push(TursoStore::new_with_options(work_path, turso_options(config)).await?);
            }
            anyhow::Ok(stores)
        })?;
        let outcome = measure_concurrent(config, writers, || {
            runtime.block_on(run_turso_round(&stores, &dataset.writer_changesets))
        });
        results.push(Measurement::concurrent(
            "concurrent write",
            "bdk_turso",
            writers,
            outcome,
        ));
        drop(stores);
    }

    runtime.block_on(async {
        let store = TursoStore::new_with_options(open_path, turso_options(config)).await?;
        store.migrate().await?;
        store.write_changeset(&dataset.changeset).await?;
        anyhow::Ok(())
    })?;
    let open = measure(config, config.open_batch, |operations| {
        for _ in 0..operations {
            runtime.block_on(async {
                let store = TursoStore::new_with_options(open_path, turso_options(config))
                    .await
                    .unwrap();
                drop(store);
            });
        }
    });
    results.push(Measurement::sequential(
        "open existing database",
        "bdk_turso",
        open,
    ));

    runtime.block_on(async {
        let store = TursoStore::new_with_options(size_path, turso_options(config)).await?;
        store.migrate().await?;
        for changeset in &dataset.history {
            store.write_changeset(changeset).await?;
        }
        anyhow::Ok(())
    })?;
    let bytes = directory_size(directory.path(), "size-turso")?;
    storage.push(StorageMeasurement {
        backend: "bdk_turso",
        bytes,
    });

    Ok(())
}

async fn run_turso_round(
    stores: &[TursoStore],
    changesets: &[Arc<ChangeSet>],
) -> Result<(), String> {
    let writers = stores.len();
    if writers == 1 {
        return stores[0]
            .write_changeset(&changesets[0])
            .await
            .map_err(|error| error.to_string());
    }

    use tokio::sync::Barrier;

    let barrier = Arc::new(Barrier::new(writers));
    let mut tasks = Vec::with_capacity(writers);
    for writer in 0..writers {
        let store = stores[writer].clone();
        let changeset = Arc::clone(&changesets[writer]);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .write_changeset(&changeset)
                .await
                .map_err(|error| error.to_string())
        }));
    }

    let mut first_error = None;
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
            }
            Err(error) => {
                first_error.get_or_insert_with(|| format!("writer task failed: {error}"));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn benchmark_sqlite(
    runtime: &Runtime,
    directory: &TempDir,
    dataset: &Dataset,
    config: &Config,
    results: &mut Vec<Measurement>,
    storage: &mut Vec<StorageMeasurement>,
) -> anyhow::Result<()> {
    let work_path = directory.path().join("work-sqlite.db");
    let open_path = directory.path().join("open-sqlite.db");
    let size_path = directory.path().join("size-sqlite.db");
    let work_path = path_str(&work_path);
    let open_path = path_str(&open_path);
    let size_path = path_str(&size_path);

    let store = runtime.block_on(async {
        let store = SqliteStore::new(work_path).await?;
        store.migrate().await?;
        store.write_changeset(&dataset.changeset).await?;
        anyhow::Ok(store)
    })?;

    let changeset = Arc::clone(&dataset.changeset);
    let write = measure(config, config.write_batch, |operations| {
        runtime.block_on(async {
            for _ in 0..operations {
                store.write_changeset(black_box(&changeset)).await.unwrap();
            }
        });
    });
    results.push(Measurement::sequential(
        "write changeset",
        "bdk_sqlite",
        write,
    ));

    let read = measure(config, config.read_batch, |operations| {
        runtime.block_on(async {
            for _ in 0..operations {
                let loaded = store.read_changeset().await.unwrap();
                black_box(loaded);
            }
        });
    });
    results.push(Measurement::sequential(
        "read changeset",
        "bdk_sqlite",
        read,
    ));

    for &writers in &config.writers {
        let outcome = measure_concurrent(config, writers, || {
            runtime.block_on(run_sqlite_round(
                &store,
                &dataset.writer_changesets,
                writers,
            ))
        });
        results.push(Measurement::concurrent(
            "concurrent write",
            "bdk_sqlite",
            writers,
            outcome,
        ));
    }
    drop(store);

    runtime.block_on(async {
        let store = SqliteStore::new(open_path).await?;
        store.migrate().await?;
        store.write_changeset(&dataset.changeset).await?;
        anyhow::Ok(())
    })?;
    let open = measure(config, config.open_batch, |operations| {
        for _ in 0..operations {
            runtime.block_on(async {
                let store = SqliteStore::new(open_path).await.unwrap();
                drop(store);
            });
        }
    });
    results.push(Measurement::sequential(
        "open existing database",
        "bdk_sqlite",
        open,
    ));

    runtime.block_on(async {
        let store = SqliteStore::new(size_path).await?;
        store.migrate().await?;
        for changeset in &dataset.history {
            store.write_changeset(changeset).await?;
        }
        anyhow::Ok(())
    })?;
    let bytes = directory_size(directory.path(), "size-sqlite")?;
    storage.push(StorageMeasurement {
        backend: "bdk_sqlite",
        bytes,
    });

    Ok(())
}

async fn run_sqlite_round(
    store: &SqliteStore,
    changesets: &[Arc<ChangeSet>],
    writers: usize,
) -> Result<(), String> {
    if writers == 1 {
        return store
            .write_changeset(&changesets[0])
            .await
            .map_err(|error| error.to_string());
    }

    use tokio::sync::Barrier;

    let barrier = Arc::new(Barrier::new(writers));
    let mut tasks = Vec::with_capacity(writers);
    for writer in 0..writers {
        let store = store.clone();
        let changeset = Arc::clone(&changesets[writer]);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .write_changeset(&changeset)
                .await
                .map_err(|error| error.to_string())
        }));
    }

    let mut first_error = None;
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
            }
            Err(error) => {
                first_error.get_or_insert_with(|| format!("writer task failed: {error}"));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn benchmark_redb(
    directory: &TempDir,
    dataset: &Dataset,
    config: &Config,
    results: &mut Vec<Measurement>,
    storage: &mut Vec<StorageMeasurement>,
) -> anyhow::Result<()> {
    let work_path = directory.path().join("work-redb.db");
    let open_path = directory.path().join("open-redb.db");
    let size_path = directory.path().join("size-redb.db");

    create_redb(&work_path)?;
    let store = Arc::new(open_redb(&work_path)?);
    store.persist_wallet(&dataset.changeset)?;

    let changeset = Arc::clone(&dataset.changeset);
    let write = measure(config, config.write_batch, |operations| {
        for _ in 0..operations {
            store.persist_wallet(black_box(&changeset)).unwrap();
        }
    });
    results.push(Measurement::sequential(
        "write changeset",
        "bdk_redb",
        write,
    ));

    let read = measure(config, config.read_batch, |operations| {
        for _ in 0..operations {
            let mut loaded = ChangeSet::default();
            store.read_wallet(&mut loaded).unwrap();
            black_box(loaded);
        }
    });
    results.push(Measurement::sequential("read changeset", "bdk_redb", read));

    for &writers in &config.writers {
        let outcome = measure_concurrent(config, writers, || {
            run_redb_round(&store, &dataset.writer_changesets, writers)
        });
        results.push(Measurement::concurrent(
            "concurrent write",
            "bdk_redb",
            writers,
            outcome,
        ));
    }
    drop(store);

    create_redb(&open_path)?;
    let open = measure(config, config.open_batch, |operations| {
        for _ in 0..operations {
            let store = open_redb(&open_path).unwrap();
            drop(store);
        }
    });
    results.push(Measurement::sequential(
        "open existing database",
        "bdk_redb",
        open,
    ));

    create_redb(&size_path)?;
    let size_store = open_redb(&size_path)?;
    for changeset in &dataset.history {
        size_store.persist_wallet(changeset)?;
    }
    drop(size_store);
    let bytes = directory_size(directory.path(), "size-redb")?;
    storage.push(StorageMeasurement {
        backend: "bdk_redb",
        bytes,
    });

    Ok(())
}

fn run_redb_round(
    store: &Arc<RedbStore>,
    changesets: &[Arc<ChangeSet>],
    writers: usize,
) -> Result<(), String> {
    if writers == 1 {
        return store
            .persist_wallet(&changesets[0])
            .map_err(|error| error.to_string());
    }

    use std::sync::Barrier;

    let barrier = Arc::new(Barrier::new(writers));
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(writers);
        for writer in 0..writers {
            let store = Arc::clone(store);
            let changeset = Arc::clone(&changesets[writer]);
            let barrier = Arc::clone(&barrier);
            handles.push(scope.spawn(move || {
                barrier.wait();
                store
                    .persist_wallet(&changeset)
                    .map_err(|error| error.to_string())
            }));
        }

        let mut first_error = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    first_error.get_or_insert(error);
                }
                Err(_) => {
                    first_error.get_or_insert_with(|| "writer thread panicked".to_owned());
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    })
}

fn create_redb(path: &Path) -> anyhow::Result<()> {
    let database = Arc::new(RedbDatabase::create(path)?);
    let store = RedbStore::new(database, REDB_WALLET_NAME.to_owned())?;
    store.create_tables::<ConfirmationBlockTime>()?;
    Ok(())
}

fn open_redb(path: &Path) -> anyhow::Result<RedbStore> {
    let database = Arc::new(RedbDatabase::open(path)?);
    RedbStore::new(database, REDB_WALLET_NAME.to_owned()).map_err(Into::into)
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("temporary path is valid UTF-8")
}

/// One measured workload result.
struct Measurement {
    workload: &'static str,
    backend: &'static str,
    writers: Option<usize>,
    result: Result<MeasurementValues, String>,
}

struct MeasurementValues {
    stats: Stats,
    throughput: f64,
}

impl Measurement {
    fn sequential(workload: &'static str, backend: &'static str, stats: Stats) -> Self {
        let throughput = 1.0 / stats.median.as_secs_f64();
        Self {
            workload,
            backend,
            writers: None,
            result: Ok(MeasurementValues { stats, throughput }),
        }
    }

    fn concurrent(
        workload: &'static str,
        backend: &'static str,
        writers: usize,
        outcome: ConcurrentOutcome,
    ) -> Self {
        let result = match outcome {
            ConcurrentOutcome::Measured { stats, throughput } => {
                Ok(MeasurementValues { stats, throughput })
            }
            ConcurrentOutcome::Failed(error) => Err(error),
        };
        Self {
            workload,
            backend,
            writers: Some(writers),
            result,
        }
    }
}

enum ConcurrentOutcome {
    Measured { stats: Stats, throughput: f64 },
    Failed(String),
}

struct StorageMeasurement {
    backend: &'static str,
    bytes: u64,
}

/// Per-operation latency statistics.
#[derive(Clone, Copy)]
struct Stats {
    median: Duration,
    p95: Duration,
}

impl Stats {
    fn from_sorted(values: &[Duration]) -> Self {
        let median = values[values.len() / 2];
        let p95 = values[(values.len() * 95)
            .div_ceil(100)
            .saturating_sub(1)
            .min(values.len() - 1)];
        Self { median, p95 }
    }
}

/// Measure `operations` sequential operations per sample and report per-operation latency.
fn measure<F>(config: &Config, operations: usize, mut run: F) -> Stats
where
    F: FnMut(usize),
{
    for _ in 0..config.warmup_samples {
        run(operations);
    }

    let mut samples = Vec::with_capacity(config.samples);
    for _ in 0..config.samples {
        let start = Instant::now();
        run(operations);
        let elapsed = start.elapsed().as_secs_f64() * 1e9 / operations as f64;
        samples.push(Duration::from_secs_f64(elapsed / 1e9));
    }
    samples.sort_unstable();
    Stats::from_sorted(&samples)
}

/// Measure rounds of concurrent writes and report per-write latency plus aggregate throughput.
///
/// A backend that rejects a write under contention stops the measurement for that writer count and
/// its error is reported instead of aborting the whole suite.
fn measure_concurrent<F>(config: &Config, writers: usize, mut run_round: F) -> ConcurrentOutcome
where
    F: FnMut() -> Result<(), String>,
{
    for _ in 0..config.warmup_samples {
        for _ in 0..config.concurrent_rounds {
            if let Err(error) = run_round() {
                return ConcurrentOutcome::Failed(error);
            }
        }
    }

    let mut rounds = Vec::with_capacity(config.samples * config.concurrent_rounds);
    for _ in 0..config.samples {
        for _ in 0..config.concurrent_rounds {
            let start = Instant::now();
            if let Err(error) = run_round() {
                return ConcurrentOutcome::Failed(error);
            }
            rounds.push(start.elapsed());
        }
    }
    rounds.sort_unstable();

    let median_round = rounds[rounds.len() / 2];
    let per_write = Stats {
        median: median_round / writers as u32,
        p95: rounds[(rounds.len() * 95)
            .div_ceil(100)
            .saturating_sub(1)
            .min(rounds.len() - 1)]
            / writers as u32,
    };
    ConcurrentOutcome::Measured {
        stats: per_write,
        throughput: writers as f64 / median_round.as_secs_f64(),
    }
}

fn render_report(
    config: &Config,
    results: &[Measurement],
    storage: &[StorageMeasurement],
) -> String {
    let mut report = String::new();
    report.push_str("# bdk_turso benchmark report\n\n");
    report.push_str(&format!(
        "Generated on {} with {} samples and {} warmup samples{}.\n\n",
        timestamp(),
        config.samples,
        config.warmup_samples,
        if config.quick { " (quick mode)" } else { "" }
    ));
    report.push_str(&render_environment(config));
    report.push_str(&render_configuration(config));
    report.push_str("\n## Workload timings\n\n");
    report.push_str(
        "| Workload | Backend | Writers | Median | p95 | Throughput | Notes |\n\
         | --- | --- | ---: | ---: | ---: | ---: | --- |\n",
    );
    for measurement in results {
        let writers = measurement
            .writers
            .map(|writers| writers.to_string())
            .unwrap_or_else(|| "-".to_owned());
        match &measurement.result {
            Ok(values) => report.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | |\n",
                measurement.workload,
                measurement.backend,
                writers,
                format_duration(values.stats.median),
                format_duration(values.stats.p95),
                format_throughput(values.throughput),
            )),
            Err(error) => report.push_str(&format!(
                "| {} | {} | {} | - | - | - | failed: {} |\n",
                measurement.workload,
                measurement.backend,
                writers,
                escape_table(error),
            )),
        }
    }

    report.push_str("\n## On-disk storage\n\n");
    report.push_str(
        "Total bytes of all files belonging to the seeded database, measured immediately after \
         the final commit. SQLite-compatible files include their `-wal` and `-shm` sidecars; no \
         checkpoint or vacuum is performed.\n\n",
    );
    report.push_str(&format!(
        "Dataset: {} distinct wallet changesets ({} transactions each).\n\n",
        config.storage_changesets, config.dataset.transactions
    ));
    report.push_str("| Backend | Bytes | MiB |\n| --- | ---: | ---: |\n");
    for measurement in storage {
        report.push_str(&format!(
            "| {} | {} | {:.2} |\n",
            measurement.backend,
            measurement.bytes,
            measurement.bytes as f64 / (1024.0 * 1024.0)
        ));
    }

    report.push_str("\n## Workload definition\n\n");
    report.push_str(&format!(
        "- bdk_turso is opened with `Options::new().with_multiprocess_wal()` when \
         `turso.multiprocess_wal` is enabled; all other experimental options are disabled. Each \
         Turso concurrent writer opens its own database handle on the same file. The other \
         backends use their default constructors.\n\
         - Each changeset contains {} transactions, {} local-chain entries, {} cached scripts per \
         descriptor, and {} locked outpoint(s), plus both descriptors and the network.\n\
         - Write and read measurements reuse the same changeset, so the database stops growing \
         after the warmup phase and later samples measure steady-state updates.\n\
         - Concurrent write rounds start all writers from a barrier and persist one distinct \
         changeset per writer. Reported latency is per write; throughput is writers divided by \
         the median round time.\n\
         - Open measurements open an existing database that already contains the schema and seed \
         data. They measure the backend's open path, not schema migration.\n",
        config.dataset.transactions,
        config.dataset.blocks,
        config.dataset.transactions * config.dataset.scripts_per_transaction,
        config.dataset.locked_outpoints,
    ));
    report
}

fn render_configuration(config: &Config) -> String {
    let writers = config
        .writers
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let rows = [
        ("Backends", config.backends.list()),
        ("samples", config.samples.to_string()),
        ("warmup_samples", config.warmup_samples.to_string()),
        ("write_batch", config.write_batch.to_string()),
        ("read_batch", config.read_batch.to_string()),
        ("concurrent_rounds", config.concurrent_rounds.to_string()),
        ("open_batch", config.open_batch.to_string()),
        ("writers", writers),
        ("storage_changesets", config.storage_changesets.to_string()),
        (
            "dataset.transactions",
            config.dataset.transactions.to_string(),
        ),
        ("dataset.blocks", config.dataset.blocks.to_string()),
        (
            "dataset.scripts_per_transaction",
            config.dataset.scripts_per_transaction.to_string(),
        ),
        (
            "dataset.locked_outpoints",
            config.dataset.locked_outpoints.to_string(),
        ),
        (
            "turso.multiprocess_wal",
            config.turso.multiprocess_wal.to_string(),
        ),
    ];

    let mut report = String::from("\n## Configuration\n\n| Setting | Value |\n| --- | --- |\n");
    for (name, value) in rows {
        report.push_str(&format!("| {name} | {} |\n", escape_table(&value)));
    }
    report
}

fn render_environment(config: &Config) -> String {
    let turso_options = if config.turso.multiprocess_wal {
        "multiprocess WAL enabled; other experimental options disabled"
    } else {
        "multiprocess WAL disabled; other experimental options disabled"
    };
    let mut rows: Vec<(&str, String)> = vec![
        ("bdk_turso", package_version("bdk_turso")),
        ("bdk_turso options", turso_options.to_owned()),
        ("bdk_sqlite", package_version("bdk_sqlite")),
        ("bdk_redb", package_version("bdk_redb")),
        ("bdk_wallet", package_version("bdk_wallet")),
        ("bdk_chain", package_version("bdk_chain")),
        ("turso", package_version("turso")),
        ("sqlx (bdk_sqlite)", package_version("sqlx")),
        ("redb (bdk_redb)", package_version("redb")),
        (
            "Rust",
            command_output("rustc", &["-V"]).unwrap_or_else(|| "unknown".to_owned()),
        ),
        (
            "Cargo",
            command_output("cargo", &["-V"]).unwrap_or_else(|| "unknown".to_owned()),
        ),
        (
            "Crate features",
            format!(
                "wallet={}, psbt={}, remote={}, blocking={}",
                cfg!(feature = "wallet"),
                cfg!(feature = "psbt"),
                cfg!(feature = "remote"),
                cfg!(feature = "blocking"),
            ),
        ),
        (
            "Profile",
            format!("cargo bench (debug assertions: {})", cfg!(debug_assertions)),
        ),
        (
            "Operating system",
            command_output("uname", &["-srm"]).unwrap_or_else(|| std::env::consts::OS.to_owned()),
        ),
        ("Architecture", std::env::consts::ARCH.to_owned()),
        ("CPU", cpu_model().unwrap_or_else(|| "unknown".to_owned())),
        ("Memory", memory().unwrap_or_else(|| "unknown".to_owned())),
        (
            "Git commit",
            command_output("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned()),
        ),
        (
            "Git status",
            if command_output("git", &["status", "--porcelain"])
                .is_some_and(|status| !status.is_empty())
            {
                "dirty".to_owned()
            } else {
                "clean".to_owned()
            },
        ),
        (
            "Async runtime",
            format!(
                "Tokio multi-thread, {} worker threads",
                config.max_writers()
            ),
        ),
    ];
    rows.sort_unstable_by_key(|(name, _)| *name);

    let mut report = String::from("## Environment\n\n| Item | Value |\n| --- | --- |\n");
    for (name, value) in rows {
        report.push_str(&format!("| {name} | {} |\n", escape_table(&value)));
    }
    report
}

fn package_version(name: &str) -> String {
    let Ok(lock) = fs::read_to_string("Cargo.lock") else {
        return "unknown".to_owned();
    };
    for package in lock.split("[[package]]") {
        let mut lines = package.lines();
        let mut found_name = false;
        let mut version = None;
        for line in lines.by_ref() {
            let line = line.trim();
            if line == format!("name = \"{name}\"") {
                found_name = true;
            } else if found_name && let Some(value) = line.strip_prefix("version = \"") {
                version = value.strip_suffix('"').map(str::to_owned);
                break;
            }
        }
        if let Some(version) = version {
            return version;
        }
    }
    "unknown".to_owned()
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn cpu_model() -> Option<String> {
    if cfg!(target_os = "macos") {
        return command_output("sysctl", &["-n", "machdep.cpu.brand_string"]);
    }
    if cfg!(target_os = "linux") {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
        return cpuinfo.lines().find_map(|line| {
            line.strip_prefix("model name")
                .and_then(|rest| rest.split_once(':'))
                .map(|(_, value)| value.trim().to_owned())
        });
    }
    None
}

fn memory() -> Option<String> {
    if cfg!(target_os = "macos") {
        let bytes = command_output("sysctl", &["-n", "hw.memsize"])?
            .parse::<u64>()
            .ok()?;
        return Some(format!(
            "{:.1} GiB",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        ));
    }
    if cfg!(target_os = "linux") {
        let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
        let kib = meminfo
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))?
            .trim()
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?;
        return Some(format!("{:.1} GiB", kib as f64 / (1024.0 * 1024.0)));
    }
    None
}

fn timestamp() -> String {
    command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .unwrap_or_else(|| "unknown time".to_owned())
}

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn format_duration(duration: Duration) -> String {
    let nanos = duration.as_secs_f64() * 1e9;
    if nanos < 1_000.0 {
        format!("{nanos:.0} ns")
    } else if nanos < 1_000_000.0 {
        format!("{:.2} µs", nanos / 1_000.0)
    } else if nanos < 1_000_000_000.0 {
        format!("{:.2} ms", nanos / 1_000_000.0)
    } else {
        format!("{:.2} s", nanos / 1_000_000_000.0)
    }
}

fn format_throughput(operations_per_second: f64) -> String {
    if operations_per_second >= 1_000_000.0 {
        format!("{:.0}/s", operations_per_second)
    } else if operations_per_second >= 1_000.0 {
        format!("{:.1}k/s", operations_per_second / 1_000.0)
    } else {
        format!("{operations_per_second:.1}/s")
    }
}

fn directory_size(directory: &Path, prefix: &str) -> anyhow::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(prefix) {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

fn descriptors() -> (
    Descriptor<DescriptorPublicKey>,
    Descriptor<DescriptorPublicKey>,
) {
    (
        EXTERNAL_DESCRIPTOR.parse().unwrap(),
        INTERNAL_DESCRIPTOR.parse().unwrap(),
    )
}

fn transaction(seed: u64, index: u32) -> Arc<Transaction> {
    let mut txid_bytes = [0u8; 32];
    txid_bytes[..8].copy_from_slice(&seed.to_le_bytes());
    txid_bytes[8..12].copy_from_slice(&index.to_le_bytes());
    let mut previous_bytes = [0u8; 32];
    previous_bytes[..8].copy_from_slice(&seed.to_be_bytes());
    previous_bytes[8..12].copy_from_slice(&index.to_be_bytes());

    Arc::new(Transaction {
        version: transaction::Version::ONE,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array(previous_bytes), 0),
            ..TxIn::default()
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(50_000 + u64::from(index)),
                script_pubkey: script(seed, index, 0),
            },
            TxOut {
                value: Amount::from_sat(25_000 + u64::from(index)),
                script_pubkey: script(seed, index, 1),
            },
        ],
    })
}

fn script(seed: u64, index: u32, output: u8) -> ScriptBuf {
    let mut bytes = vec![0x00, 0x14];
    bytes.extend_from_slice(&seed.to_le_bytes());
    bytes.extend_from_slice(&index.to_le_bytes());
    bytes.push(output);
    bytes.extend_from_slice(&[seed as u8; 12]);
    ScriptBuf::from_bytes(bytes)
}

fn full_changeset(seed: u64, config: &DatasetConfig) -> ChangeSet {
    let (external, internal) = descriptors();
    let external_id = external.descriptor_id();
    let internal_id = internal.descriptor_id();

    let mut tx_graph = tx_graph::ChangeSet::default();
    let mut spk_cache: BTreeMap<DescriptorId, BTreeMap<u32, ScriptBuf>> = BTreeMap::new();
    let mut locked_outpoints = locked_outpoints::ChangeSet::default();

    let blocks = (0..config.blocks)
        .map(|height| {
            (
                height,
                Some(BlockHash::from_byte_array(block_bytes(seed, height))),
            )
        })
        .collect();

    for index in 0..config.transactions {
        let tx = transaction(seed, index);
        let txid = tx.compute_txid();
        let block_hash = BlockHash::from_byte_array(block_bytes(seed, index));
        let anchor = ConfirmationBlockTime {
            block_id: BlockId {
                height: 100 + index,
                hash: block_hash,
            },
            confirmation_time: 1_700_000_000 + seed + u64::from(index),
        };

        tx_graph.txs.insert(Arc::clone(&tx));
        for vout in 0..2 {
            tx_graph
                .txouts
                .insert(OutPoint::new(txid, vout), tx.output[vout as usize].clone());
        }
        tx_graph.anchors.insert((anchor, txid));
        tx_graph
            .first_seen
            .insert(txid, 1_000 + seed + u64::from(index));
        tx_graph
            .last_seen
            .insert(txid, 2_000 + seed + u64::from(index));
        tx_graph
            .last_evicted
            .insert(txid, 3_000 + seed + u64::from(index));

        if index < config.locked_outpoints {
            locked_outpoints
                .outpoints
                .insert(OutPoint::new(txid, index % 2), true);
        }

        let external_scripts = spk_cache.entry(external_id).or_default();
        for offset in 0..config.scripts_per_transaction {
            let derivation_index = index * config.scripts_per_transaction + offset;
            external_scripts.insert(derivation_index, script(seed, derivation_index, 0));
        }
        let internal_scripts = spk_cache.entry(internal_id).or_default();
        for offset in 0..config.scripts_per_transaction {
            let derivation_index = index * config.scripts_per_transaction + offset;
            internal_scripts.insert(derivation_index, script(seed, derivation_index, 1));
        }
    }

    let revealed = config.transactions * config.scripts_per_transaction;

    ChangeSet {
        descriptor: Some(external),
        change_descriptor: Some(internal),
        network: Some(Network::Signet),
        local_chain: local_chain::ChangeSet { blocks },
        tx_graph,
        indexer: keychain_txout::ChangeSet {
            last_revealed: [(external_id, revealed), (internal_id, revealed)].into(),
            spk_cache,
        },
        locked_outpoints,
    }
}

fn block_bytes(seed: u64, index: u32) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    bytes[8..16].copy_from_slice(&u64::from(index).to_le_bytes());
    bytes[16..].fill(seed as u8 ^ index as u8);
    bytes
}
