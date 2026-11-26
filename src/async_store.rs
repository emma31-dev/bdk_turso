//! Async Turso storage for BDK wallet and chain changesets.
//!
//! [`Store`] owns a Turso database connection pool and exposes methods to migrate the schema and
//! read or write persisted changesets.

use std::{fmt, sync::Arc, time::Duration};

use bdk_chain::{
    BlockId, ConfirmationBlockTime, DescriptorId, bitcoin, keychain_txout, local_chain, tx_graph,
};
use bitcoin::hex::DisplayHex;
use bitcoin::{Amount, BlockHash, OutPoint, ScriptBuf, Transaction, TxOut, Txid, consensus};
pub use turso::EncryptionOpts;
#[cfg(feature = "remote")]
pub use turso::sync::{PartialBootstrapStrategy, PartialSyncOpts, RemoteEncryptionCipher};
pub use turso::transaction::TransactionBehavior;
use turso::{
    Builder, Connection, Database as LocalDatabase, Value,
    transaction::Transaction as DatabaseTransaction,
};

use crate::Error;

/// A column as reported by `PRAGMA table_info`: name, declared type, not-null flag, default value,
/// and primary-key position.
type TableColumn = (String, String, i64, Value, i64);

/// How long a connection waits for a contended database lock before returning an error.
///
/// Turso leaves its busy handler unset by default, so a concurrent writer otherwise fails
/// immediately with `Busy`. This matches the 5-second default used by `sqlx`'s SQLite driver and
/// can be overridden with [`Options::with_busy_timeout`].
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Tables whose rows are deleted by [`Store::wipe`]. Feature-gated tables are still cleared when
/// they exist in the database, even if the current build does not create them.
const MANAGED_TABLES: &[&str] = &[
    "anchor",
    "block",
    "keychain",
    "keychain_last_revealed",
    "keychain_script_pubkey",
    "locked_outpoint",
    "network",
    "psbt",
    "tx",
    "txout",
];

/// Switch a local database to MVCC journal mode.
///
/// `BEGIN CONCURRENT` is only accepted when MVCC is enabled. The setting is persisted in the
/// database header, so this is a no-op after the first call.
async fn enable_mvcc(database: &LocalDatabase) -> Result<(), Error> {
    let connection = database.connect()?;
    let mut rows = connection.query("PRAGMA journal_mode = 'mvcc'", ()).await?;
    while rows.next().await?.is_some() {}
    Ok(())
}

/// Configuration applied when opening a [`Store`].
///
/// Configure an `Options` value before opening the database; these settings cannot be changed on
/// an already-open database. Options cover the busy timeout, transaction behavior, local
/// encryption, and remote sync settings.
///
/// Turso engine capabilities that only affect application SQL, such as `ATTACH DATABASE`,
/// `VACUUM`, generated columns, `WITHOUT ROWID`, and alternative index methods, are deliberately
/// not exposed. Open your own Turso database or run the corresponding SQL if an application needs
/// them. Engine flags that follow from the transaction behavior are enabled automatically:
/// [`TransactionBehavior::Concurrent`] turns on MVCC passive checkpointing, and the other
/// behaviors turn on multi-process WAL for file-backed local databases.
#[derive(Clone, Default)]
pub struct Options {
    /// How long a connection waits for a contended database lock before returning an error.
    busy_timeout: Option<Duration>,
    /// Transaction behavior applied to every connection opened by the store.
    transaction_behavior: Option<TransactionBehavior>,
    /// Enables local at-rest encryption and supplies its cipher and key.
    encryption: Option<EncryptionOpts>,
    #[cfg(feature = "remote")]
    /// Enables Turso Cloud encryption; this does not encrypt the local sync cache.
    remote_encryption: Option<(String, RemoteEncryptionCipher)>,
    #[cfg(feature = "remote")]
    /// Overrides Turso's empty-database bootstrap behavior for remote caches.
    bootstrap_if_empty: Option<bool>,
    #[cfg(feature = "remote")]
    /// Long-poll timeout used while waiting for remote changes.
    long_poll_timeout: Option<Duration>,
    #[cfg(feature = "remote")]
    /// Client name reported to the Turso sync engine.
    client_name: Option<String>,
    #[cfg(feature = "remote")]
    /// Async auth-token provider applied before every sync request.
    auth_token_fn: Option<turso::sync::AuthTokenFn>,
    #[cfg(feature = "remote")]
    /// Experimental partial-sync configuration.
    partial_sync_opts: Option<PartialSyncOpts>,
    #[cfg(feature = "remote")]
    /// Overrides the incremental pull protocol.
    logical_mvcc_pull: Option<bool>,
}

impl Options {
    /// Create options with every optional setting disabled.
    ///
    /// Enable only the capabilities needed by the application by chaining the relevant `with_*`
    /// methods before passing the options to a store constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set how long a connection waits for a contended database lock before returning `Busy`.
    ///
    /// The default is 5 seconds, matching the default used by `sqlx`'s SQLite driver. A longer
    /// timeout lets a writer wait out a short-lived lock instead of failing immediately; a shorter
    /// timeout returns control to the caller sooner. Pass [`Duration::ZERO`] to fail as soon as the
    /// lock is contended rather than waiting.
    ///
    /// The timeout applies to every connection the store opens, including the local cache of a
    /// remote sync store.
    pub fn with_busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = Some(timeout);
        self
    }

    /// Set the behavior used by every transaction opened through this store.
    ///
    /// Turso stores one [`TransactionBehavior`] per connection, and a later call replaces the
    /// earlier value, so the four behaviors are mutually exclusive rather than composable. Calling
    /// this method more than once is allowed and the last call wins.
    ///
    /// - [`TransactionBehavior::Deferred`] (the default) starts the transaction on first access.
    /// - [`TransactionBehavior::Immediate`] acquires the write lock at `BEGIN`.
    /// - [`TransactionBehavior::Exclusive`] prevents other connections from reading while the
    ///   transaction is open.
    /// - [`TransactionBehavior::Concurrent`] allows parallel writers with conflicts detected at
    ///   commit.
    ///
    /// Selecting `Concurrent` switches a local database to MVCC journal mode when the store is
    /// opened and enables Turso's MVCC passive-checkpoint behavior. Turso rejects DDL inside
    /// `BEGIN CONCURRENT`, so [`Store::migrate`] and [`Store::migrate_to`] still use a
    /// `Deferred` transaction for schema changes. `Concurrent` is rejected by remote sync stores.
    /// The other behaviors enable Turso's multi-process WAL coordinator for file-backed local
    /// databases, so several operating-system processes can share the same database file. The
    /// crate's read and write queries otherwise run under the selected behavior.
    pub fn with_transaction_behavior(mut self, behavior: TransactionBehavior) -> Self {
        self.transaction_behavior = Some(behavior);
        self
    }

    /// Configure local at-rest encryption using Turso's cipher name and hexadecimal key.
    ///
    /// Keep the key secret and use the same settings whenever reopening the database. Encrypted
    /// local files cannot be opened by SQLite-compatible backends. Remote sync stores cannot use
    /// local at-rest encryption; use `with_remote_encryption` for Turso Cloud encryption instead.
    /// Cloud encryption does not encrypt the local sync cache.
    pub fn with_encryption(mut self, encryption: EncryptionOpts) -> Self {
        self.encryption = Some(encryption);
        self
    }

    /// Configure Turso Cloud encryption with a base64-encoded key and cipher.
    ///
    /// This setting is available with the `remote` feature and only applies to remote stores. It
    /// enables Turso Cloud database encryption; the local sync cache remains unencrypted.
    #[cfg(feature = "remote")]
    pub fn with_remote_encryption(
        mut self,
        base64_key: impl Into<String>,
        cipher: RemoteEncryptionCipher,
    ) -> Self {
        self.remote_encryption = Some((base64_key.into(), cipher));
        self
    }

    /// Control whether an empty local cache downloads the remote schema and data.
    ///
    /// Turso bootstraps an empty cache by default. Pass `false` for a deferred bootstrap, for
    /// example when the application wants to inspect or pre-seed the cache before the first sync.
    /// This setting is available with the `remote` feature and is rejected by local stores.
    #[cfg(feature = "remote")]
    pub fn with_bootstrap_if_empty(mut self, enable: bool) -> Self {
        self.bootstrap_if_empty = Some(enable);
        self
    }

    /// Set how long the sync engine long-polls for remote changes.
    ///
    /// A longer timeout reduces request frequency while waiting for changes; a shorter timeout
    /// returns control to the caller sooner. This setting is available with the `remote` feature,
    /// only applies to remote stores, and is rejected by local stores.
    #[cfg(feature = "remote")]
    pub fn with_long_poll_timeout(mut self, timeout: Duration) -> Self {
        self.long_poll_timeout = Some(timeout);
        self
    }

    /// Set the client name reported to the Turso sync engine.
    ///
    /// Turso defaults to `turso-sync-rust`. Use a stable application-specific name so server-side
    /// telemetry and tracing can distinguish clients. This setting is available with the `remote`
    /// feature, only applies to remote stores, and is rejected by local stores.
    #[cfg(feature = "remote")]
    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = Some(name.into());
        self
    }

    /// Provide an async callback that returns a fresh auth token for every sync request.
    ///
    /// Use this for rotating credentials instead of passing a static token to
    /// [`Store::new_remote`]. The callback is invoked before each HTTP request, and an error it
    /// returns fails the in-flight sync operation. Calling this overrides the static token supplied
    /// to [`Store::new_remote_with_options`]. This setting is available with the `remote` feature,
    /// only applies to remote stores, and is rejected by local stores.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # #[cfg(feature = "remote")]
    /// # fn example() {
    /// use bdk_turso::Options;
    ///
    /// let options = Options::new().with_auth_token_fn(|| async {
    ///     Ok::<_, turso::Error>("fresh-token".to_owned())
    /// });
    /// # }
    /// ```
    #[cfg(feature = "remote")]
    pub fn with_auth_token_fn<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<String, turso::Error>> + Send + 'static,
    {
        self.auth_token_fn = Some(Arc::new(move || -> turso::sync::AuthTokenFut {
            Box::pin(f())
        }));
        self
    }

    /// Configure experimental partial synchronization for the local cache.
    ///
    /// Partial sync downloads only a segment of the remote database, which bounds cache size for
    /// large databases. This is a Turso experimental feature; [`PartialSyncOpts`] and
    /// [`PartialBootstrapStrategy`] are re-exported from Turso. This setting is available with the
    /// `remote` feature, only applies to remote stores, and is rejected by local stores.
    #[cfg(feature = "remote")]
    pub fn with_partial_sync_opts_experimental(mut self, opts: PartialSyncOpts) -> Self {
        self.partial_sync_opts = Some(opts);
        self
    }

    /// Force the incremental pull protocol instead of letting Turso auto-detect it.
    ///
    /// `true` forces MVCC logical-log pulls and `false` forces page-stream pulls. Turso normally
    /// detects the remote protocol on the first pull and persists the result, so this is only an
    /// escape hatch for tests or unusual deployments. This setting is available with the `remote`
    /// feature, only applies to remote stores, and is rejected by local stores.
    #[cfg(feature = "remote")]
    pub fn with_logical_mvcc_pull(mut self, enable: bool) -> Self {
        self.logical_mvcc_pull = Some(enable);
        self
    }

    /// Return whether any remote-sync cache option was configured.
    #[cfg(feature = "remote")]
    fn has_sync_cache_options(&self) -> bool {
        self.bootstrap_if_empty.is_some()
            || self.long_poll_timeout.is_some()
            || self.client_name.is_some()
            || self.auth_token_fn.is_some()
            || self.partial_sync_opts.is_some()
            || self.logical_mvcc_pull.is_some()
    }
}

/// Generate local encryption options with a random 32-byte key and the `aes256gcm` cipher.
///
/// Save the returned `hexkey` in a secret manager before opening the database, and reuse these
/// options to reopen it; losing the key makes the data inaccessible.
///
/// ```
/// use bdk_turso::{Options, generate_encryption_opts};
///
/// let encryption = generate_encryption_opts();
/// assert_eq!(encryption.cipher, "aes256gcm");
/// assert_eq!(encryption.hexkey.len(), 64);
/// let options = Options::new().with_encryption(encryption);
/// ```
pub fn generate_encryption_opts() -> EncryptionOpts {
    let key: [u8; 32] = rand::random();
    EncryptionOpts {
        cipher: "aes256gcm".to_owned(),
        hexkey: key.as_slice().to_lower_hex_string(),
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field(
                "busy_timeout",
                &self.busy_timeout.unwrap_or(DEFAULT_BUSY_TIMEOUT),
            )
            .field(
                "transaction_behavior",
                &match self.transaction_behavior {
                    None | Some(TransactionBehavior::Deferred) => "deferred",
                    Some(TransactionBehavior::Immediate) => "immediate",
                    Some(TransactionBehavior::Exclusive) => "exclusive",
                    Some(TransactionBehavior::Concurrent) => "concurrent",
                    Some(_) => "unknown",
                },
            )
            .field("encryption_enabled", &self.encryption.is_some())
            .field("remote_encryption_enabled", &{
                #[cfg(feature = "remote")]
                {
                    self.remote_encryption.is_some()
                }
                #[cfg(not(feature = "remote"))]
                {
                    false
                }
            })
            .field("remote_cache_configured", &{
                #[cfg(feature = "remote")]
                {
                    self.has_sync_cache_options()
                }
                #[cfg(not(feature = "remote"))]
                {
                    false
                }
            })
            .finish()
    }
}

/// Statistics for a remote store's local Turso sync cache.
///
/// Returned by [`Store::cache_stats`]. Local stores do not have a sync cache, so `cache_stats`
/// returns `None` for them. Sizes are in bytes and times are Unix timestamps in seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Number of change-data-capture operations recorded in the cache.
    pub cdc_operations: i64,
    /// Size of the main write-ahead log in bytes.
    pub main_wal_size: u64,
    /// Size of the revert write-ahead log in bytes.
    pub revert_wal_size: u64,
    /// Unix time of the last successful pull, if the cache has ever pulled.
    pub last_pull_unix_time: Option<i64>,
    /// Unix time of the last successful push, if the cache has ever pushed.
    pub last_push_unix_time: Option<i64>,
    /// Remote revision last applied to the cache, if known.
    pub revision: Option<String>,
    /// Bytes sent to Turso Cloud over the sync protocol.
    pub network_sent_bytes: usize,
    /// Bytes received from Turso Cloud over the sync protocol.
    pub network_received_bytes: usize,
}

#[cfg(feature = "remote")]
impl From<turso::sync::DatabaseSyncStats> for CacheStats {
    fn from(stats: turso::sync::DatabaseSyncStats) -> Self {
        Self {
            cdc_operations: stats.cdc_operations,
            main_wal_size: stats.main_wal_size,
            revert_wal_size: stats.revert_wal_size,
            last_pull_unix_time: stats.last_pull_unix_time,
            last_push_unix_time: stats.last_push_unix_time,
            revision: stats.revision,
            network_sent_bytes: stats.network_sent_bytes,
            network_received_bytes: stats.network_received_bytes,
        }
    }
}

/// An asynchronous store for BDK wallet changesets backed by Turso.
///
/// Local stores use Turso in process. With the `remote` feature, remote stores keep a local sync
/// cache and synchronize changes with Turso Cloud. Call [`Store::migrate`] before using low-level
/// read/write methods; `AsyncWalletPersister` initialization performs migration automatically.
///
/// Do not run `bdk_sqlite::Store::migrate()` on this database. Since it has no SQLx migration
/// history, `bdk_sqlite` will run migrations containing `DROP TABLE IF EXISTS` statements and
/// erase the existing wallet state.
#[derive(Clone)]
pub struct Store {
    pub(crate) database: StoreDatabase,
    options: Option<Options>,
    cache_path: Option<String>,
}

#[derive(Clone)]
pub(crate) enum StoreDatabase {
    Local(LocalDatabase),
    #[cfg(feature = "remote")]
    Remote(turso::sync::Database),
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field(
                "backend",
                &match &self.database {
                    StoreDatabase::Local(_) => "local",
                    #[cfg(feature = "remote")]
                    StoreDatabase::Remote(_) => "remote",
                },
            )
            .field("options", &self.options)
            .finish()
    }
}

impl Store {
    /// Create an in-memory database with default options.
    ///
    /// The database exists for the lifetime of the returned store and is not persisted to a file.
    /// Use [`Store::new_memory_with_options`] when an in-memory database needs non-default
    /// options.
    pub async fn new_memory() -> Result<Self, Error> {
        Self::new_memory_with_options(Options::default()).await
    }

    /// Create an in-memory database using the supplied options.
    ///
    /// The options are applied while Turso opens the database. The database remains in memory for
    /// the lifetime of the store and is not persisted to disk.
    pub async fn new_memory_with_options(options: Options) -> Result<Self, Error> {
        Self::new_with_options(":memory:", options).await
    }

    /// Create or open a local database using default options.
    ///
    /// This constructor uses the default options. It does not create the BDK
    /// schema; call [`Store::migrate`] before using the persistence methods, unless the store is
    /// initialized through `AsyncWalletPersister`.
    pub async fn new(path: &str) -> Result<Self, Error> {
        Self::new_with_options(path, Options::default()).await
    }

    /// Create or open a local database with the supplied Turso options.
    ///
    /// The options are applied only while opening the database. Only the settings configured on
    /// `options` are enabled. This constructor does not run the BDK
    /// schema migration; call [`Store::migrate`] before using the persistence methods.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use bdk_turso::{Options, Store, TransactionBehavior};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let options = Options::new().with_transaction_behavior(TransactionBehavior::Immediate);
    /// let store = Store::new_with_options("wallet.sqlite", options).await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new_with_options(path: &str, options: Options) -> Result<Self, Error> {
        let concurrent = matches!(
            options.transaction_behavior,
            Some(TransactionBehavior::Concurrent)
        );
        // Turso rejects multi-process WAL for in-memory databases and for MVCC.
        let multiprocess_wal = !concurrent && path != ":memory:";

        #[cfg(feature = "remote")]
        if options.remote_encryption.is_some() {
            return Err(Error::UnsupportedOption(
                "remote encryption can only be used with Store::new_remote_with_options".into(),
            ));
        }

        #[cfg(feature = "remote")]
        if options.has_sync_cache_options() {
            return Err(Error::UnsupportedOption(
                "remote sync cache options can only be used with Store::new_remote_with_options"
                    .into(),
            ));
        }

        let mut builder = Builder::new_local(path)
            .experimental_multiprocess_wal(multiprocess_wal)
            .experimental_mvcc_passive_checkpoint(concurrent);

        if let Some(encryption) = &options.encryption {
            builder = builder
                .experimental_encryption(true)
                .with_encryption(encryption.clone());
        }

        let database = builder.build().await?;
        if concurrent {
            enable_mvcc(&database).await?;
        }
        Ok(Self {
            database: StoreDatabase::Local(database),
            options: Some(options),
            cache_path: None,
        })
    }

    /// Connect to a Turso Cloud database using a local sync cache.
    ///
    /// This constructor requires the `remote` feature. `local_path` is the on-device cache file,
    /// while `remote_url` and `auth_token` are supplied by the caller. Reads pull remote changes
    /// before accessing the cache, and committed writes are pushed to Turso Cloud. If a push fails
    /// after a local commit, the method returns an error but the cache may contain the write; retry
    /// the same persistence operation to retry its upload. Use a disposable remote database for
    /// integration testing.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # #[cfg(feature = "remote")]
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use bdk_turso::Store;
    ///
    /// let store = Store::new_remote(
    ///     "wallet-cache.db",
    ///     "libsql://your-database.turso.io",
    ///     "your-auth-token",
    /// ).await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "remote")]
    pub async fn new_remote(
        local_path: &str,
        remote_url: &str,
        auth_token: &str,
    ) -> Result<Self, Error> {
        Self::new_remote_with_options(local_path, remote_url, auth_token, Options::default()).await
    }

    /// Connect to Turso Cloud using a local cache and the supplied database options.
    ///
    /// Local at-rest encryption is unsupported in sync mode and returns an error. Use
    /// [`Options::with_remote_encryption`] for Turso Cloud encryption; this does not encrypt the
    /// local cache. Multi-process WAL is enabled for the local sync cache.
    #[cfg(feature = "remote")]
    pub async fn new_remote_with_options(
        local_path: &str,
        remote_url: &str,
        auth_token: &str,
        options: Options,
    ) -> Result<Self, Error> {
        if options.encryption.is_some() {
            return Err(Error::UnsupportedOption(
                "local at-rest encryption is unsupported in remote sync mode; use `with_remote_encryption` for Turso Cloud encryption (the local cache remains unencrypted)".into(),
            ));
        }
        if matches!(
            options.transaction_behavior,
            Some(TransactionBehavior::Concurrent)
        ) {
            return Err(Error::UnsupportedOption(
                "Concurrent transaction behavior requires MVCC and is not supported by Turso's remote sync builder".into(),
            ));
        }

        let mut builder = turso::sync::Builder::new_remote(local_path)
            .with_remote_url(remote_url)
            .with_auth_token(auth_token)
            .experimental_multiprocess_wal(true);

        if let Some(bootstrap_if_empty) = options.bootstrap_if_empty {
            builder = builder.bootstrap_if_empty(bootstrap_if_empty);
        }
        if let Some(timeout) = options.long_poll_timeout {
            builder = builder.with_long_poll_timeout(timeout);
        }
        if let Some(client_name) = &options.client_name {
            builder = builder.with_client_name(client_name.clone());
        }
        if let Some(auth_token_fn) = options.auth_token_fn.clone() {
            builder = builder.with_auth_token_fn(move || auth_token_fn());
        }
        if let Some(partial_sync_opts) = &options.partial_sync_opts {
            builder = builder.with_partial_sync_opts_experimental(partial_sync_opts.clone());
        }
        if let Some(logical_mvcc_pull) = options.logical_mvcc_pull {
            builder = builder.with_logical_mvcc_pull(logical_mvcc_pull);
        }
        if let Some((key, cipher)) = &options.remote_encryption {
            builder = builder.with_remote_encryption(key.clone(), *cipher);
        }

        let database = builder.build().await?;
        Ok(Self {
            database: StoreDatabase::Remote(database),
            options: Some(options),
            cache_path: Some(local_path.to_owned()),
        })
    }

    /// Wrap an already-open local Turso database in a `Store`.
    ///
    /// This does not open a path or run the BDK schema migration. Call [`Store::migrate`] before
    /// using persistence methods. Because the database was created externally, its opening options
    /// are not inferred by the store.
    pub fn from_database(database: LocalDatabase) -> Self {
        Self {
            database: StoreDatabase::Local(database),
            options: None,
            cache_path: None,
        }
    }

    /// Return the options retained by this store, if they are known.
    ///
    /// Stores created with a constructor retain the configuration passed to Turso. Stores wrapped
    /// with [`Store::from_database`] return `None` because the database's original settings cannot
    /// be inferred.
    pub fn options(&self) -> Option<&Options> {
        self.options.as_ref()
    }

    pub(crate) async fn connect(&self) -> Result<Connection, Error> {
        self.connect_with_behavior(self.configured_transaction_behavior())
            .await
    }

    fn configured_transaction_behavior(&self) -> TransactionBehavior {
        self.options
            .as_ref()
            .and_then(|options| options.transaction_behavior)
            .unwrap_or(TransactionBehavior::Deferred)
    }

    fn configured_busy_timeout(&self) -> Duration {
        self.options
            .as_ref()
            .and_then(|options| options.busy_timeout)
            .unwrap_or(DEFAULT_BUSY_TIMEOUT)
    }

    async fn connect_with_behavior(
        &self,
        transaction_behavior: TransactionBehavior,
    ) -> Result<Connection, Error> {
        let busy_timeout = self.configured_busy_timeout();
        match &self.database {
            StoreDatabase::Local(database) => {
                let mut connection = database.connect()?;
                connection.busy_timeout(busy_timeout)?;
                connection.set_transaction_behavior(transaction_behavior);
                Ok(connection)
            }
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => {
                database.pull().await?;
                let mut connection = database.connect().await?;
                connection.busy_timeout(busy_timeout)?;
                connection.set_transaction_behavior(transaction_behavior);
                Ok(connection)
            }
        }
    }

    /// Push locally committed changes from the cache to Turso Cloud.
    ///
    /// Remote persistence methods call this after committing local changes. If an upload fails,
    /// call `push` to retry it without reapplying the changeset. This is a no-op for local stores
    /// and does not pull remote changes.
    pub async fn push(&self) -> Result<(), Error> {
        match &self.database {
            StoreDatabase::Local(_) => Ok(()),
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => database.push().await.map_err(Error::RemotePush),
        }
    }

    /// Pull remote changes into the local sync cache.
    ///
    /// Returns `true` when remote changes were applied and `false` when the cache was already
    /// current. This is a no-op that returns `Ok(false)` for local stores, matching [`Store::push`].
    ///
    /// This only pulls; it does not upload local changes. Remote persistence methods already pull
    /// implicitly before every operation, so call this when you need an explicit refresh, for
    /// example before going offline or before a read-heavy phase.
    ///
    /// # Errors
    ///
    /// A failed pull returns [`Error::RemotePull`].
    pub async fn pull(&self) -> Result<bool, Error> {
        match &self.database {
            StoreDatabase::Local(_) => Ok(false),
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => database.pull().await.map_err(Error::RemotePull),
        }
    }

    /// Force a write-ahead-log checkpoint of the local sync cache.
    ///
    /// Large changesets can leave the cache's WAL files large after the data is committed. A
    /// checkpoint folds the WAL back into the main database file and reclaims that space. This is a
    /// no-op for local stores, which do not use a sync cache.
    ///
    /// Turso retries a busy checkpoint internally (up to 100 attempts at 10 ms) before returning an
    /// error. This only affects the local cache; it does not contact Turso Cloud.
    ///
    /// # Errors
    ///
    /// A failed checkpoint returns [`Error::RemoteCheckpoint`].
    pub async fn checkpoint(&self) -> Result<(), Error> {
        match &self.database {
            StoreDatabase::Local(_) => Ok(()),
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => {
                database.checkpoint().await.map_err(Error::RemoteCheckpoint)
            }
        }
    }

    /// Leave the local sync cache fully uploaded and compacted.
    ///
    /// This calls [`Store::push`] to upload pending local changes and then [`Store::checkpoint`] to
    /// fold and reclaim the cache WAL. It does not pull remote changes. For local stores both steps
    /// are no-ops, so this returns `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RemotePush`] if the upload fails, or [`Error::RemoteCheckpoint`] if the
    /// checkpoint fails. A failed push leaves the local changes in the cache for a later retry.
    pub async fn flush_cache(&self) -> Result<(), Error> {
        self.push().await?;
        self.checkpoint().await
    }

    /// Return statistics for the local sync cache, or `None` for local stores.
    ///
    /// The returned [`CacheStats`] is owned by this crate, so `turso` types do not leak into the
    /// public API. It reports WAL sizes, last pull and push times, the last applied revision, sync
    /// network bytes, and change-data-capture operations.
    ///
    /// # Errors
    ///
    /// A failed stats read returns [`Error::RemoteStats`].
    pub async fn cache_stats(&self) -> Result<Option<CacheStats>, Error> {
        match &self.database {
            StoreDatabase::Local(_) => Ok(None),
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => database
                .stats()
                .await
                .map(|stats| Some(CacheStats::from(stats)))
                .map_err(Error::RemoteStats),
        }
    }

    /// Return the path of the local sync cache file, or `None` for local stores.
    ///
    /// Use this to locate the cache for disk-usage checks or to move it with the application's data
    /// directory. The path is the `local_path` passed to [`Store::new_remote`] or
    /// [`Store::new_remote_with_options`].
    pub fn cache_path(&self) -> Option<&str> {
        self.cache_path.as_deref()
    }

    /// Apply the default schema and schemas for enabled features in a transaction.
    ///
    /// Core chain and transaction tables are always created. Wallet-specific tables are created
    /// only with the `wallet` feature, and the PSBT table only with `psbt`. This migration is safe
    /// to run repeatedly; `AsyncWalletPersister::initialize` runs it for you when enabled.
    ///
    /// DDL runs in a `Deferred` transaction even when the store uses another transaction behavior,
    /// because Turso rejects DDL inside `BEGIN CONCURRENT`.
    pub async fn migrate(&self) -> Result<(), Error> {
        let mut conn = self
            .connect_with_behavior(TransactionBehavior::Deferred)
            .await?;
        let txn = conn.transaction().await?;
        txn.execute_batch(include_str!("../migrations/0001_default_schema.up.sql"))
            .await?;
        #[cfg(feature = "wallet")]
        txn.execute_batch(include_str!("../migrations/0002_wallet_schema.up.sql"))
            .await?;
        #[cfg(feature = "psbt")]
        txn.execute_batch(include_str!("../migrations/0003_psbt.up.sql"))
            .await?;
        txn.commit().await?;
        drop(conn);
        self.push().await?;
        Ok(())
    }

    /// Delete all rows from the tables managed by this crate.
    ///
    /// Every table created by [`Store::migrate`] is cleared when it exists: `block`, `tx`,
    /// `txout`, `anchor`, `keychain`, `keychain_last_revealed`, `keychain_script_pubkey`,
    /// `network`, `locked_outpoint`, and `psbt`. Features are discovered from the database, so a
    /// table created by an earlier build with the `wallet` or `psbt` feature enabled is cleared
    /// even when the current build does not create it.
    ///
    /// All deletes run in one transaction. A failure before the commit leaves the previous data
    /// intact. The schema, indexes, connection, and options are unchanged, so the store can be
    /// used immediately after a successful wipe. Wiping a store that has not been migrated is a
    /// no-op.
    ///
    /// This is a logical wipe, not a secure erase. SQLite and Turso may retain old bytes in WAL
    /// files, free pages, filesystem snapshots, or SSD wear-leveling. Destroy the encryption key
    /// or securely erase the database files when cryptographic erasure is required.
    ///
    /// Remote stores pull before wiping, commit locally, and then push the deletion to Turso
    /// Cloud. If the upload fails, the local cache is already wiped and the returned
    /// [`Error::RemotePush`] can be retried with [`Store::push`].
    pub async fn wipe(&self) -> Result<(), Error> {
        let mut conn = self
            .connect_with_behavior(TransactionBehavior::Deferred)
            .await?;
        let txn = conn.transaction().await?;
        for table in Self::existing_managed_tables(&txn).await? {
            txn.execute(format!("DELETE FROM {}", Self::quote_identifier(table)), ())
                .await?;
        }
        txn.commit().await?;
        drop(conn);
        self.push().await?;
        Ok(())
    }

    /// Copy tables, indexes, and rows present in this store to `destination` without running schema
    /// migrations.
    ///
    /// Existing tables must have compatible column definitions. Source rows replace destination
    /// rows on key conflicts, while destination-only rows and tables remain. Indexes are copied
    /// after rows are transferred; an index that already exists in the destination must have the
    /// same definition. The destination is expected to be empty, though merging into an existing
    /// store is supported. The transfer is atomic on the destination; the source is unchanged.
    /// Feature-gated tables are discovered from the database itself, so this also works when the
    /// stores were opened with different features or wrapped with [`Store::from_database`].
    /// Internal SQLite and Turso tables are not copied. Both the source and destination are read
    /// through their own transactions, and remote stores pull before access. The destination is
    /// pushed after the local transaction commits; a failed remote push may leave the destination's
    /// local cache updated.
    ///
    /// Both connections use a `Deferred` transaction behavior, because Turso rejects DDL inside
    /// `BEGIN CONCURRENT` and this method creates tables and indexes on the destination.
    pub async fn migrate_to(&self, destination: &Store) -> Result<(), Error> {
        let mut source_conn = self
            .connect_with_behavior(TransactionBehavior::Deferred)
            .await?;
        let mut destination_conn = destination
            .connect_with_behavior(TransactionBehavior::Deferred)
            .await?;
        let (source_tables, source_columns, source_indexes) = {
            let source_txn = source_conn.transaction().await?;
            let source_tables = Self::user_tables(&source_txn).await?;
            let source_table_names = source_tables
                .iter()
                .map(|(name, _)| name.to_ascii_lowercase())
                .collect::<std::collections::HashSet<_>>();
            let mut source_columns: std::collections::HashMap<String, Vec<TableColumn>> =
                std::collections::HashMap::new();
            for (name, _) in &source_tables {
                let columns = Self::table_columns(&source_txn, name).await?;
                source_columns.insert(name.to_ascii_lowercase(), columns);
            }
            let source_indexes = Self::user_indexes(&source_txn)
                .await?
                .into_iter()
                .filter(|(_, table, _)| source_table_names.contains(&table.to_ascii_lowercase()))
                .collect::<Vec<_>>();
            source_txn.commit().await?;
            (source_tables, source_columns, source_indexes)
        };
        let destination_tables = Self::user_tables(&destination_conn).await?;
        let destination_names = destination_tables
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let mut destination_columns: std::collections::HashMap<String, Vec<TableColumn>> =
            std::collections::HashMap::new();
        for (name, _) in &destination_tables {
            let columns = Self::table_columns(&destination_conn, name).await?;
            destination_columns.insert(name.to_ascii_lowercase(), columns);
        }
        let destination_indexes = Self::user_indexes(&destination_conn)
            .await?
            .into_iter()
            .map(|(name, _, sql)| (name.to_ascii_lowercase(), sql))
            .collect::<std::collections::HashMap<_, _>>();

        let source_txn = source_conn.transaction().await?;
        let destination_txn = destination_conn.transaction().await?;
        for (table, create_sql) in source_tables {
            if create_sql
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("create virtual table")
            {
                return Err(Error::Migration(format!(
                    "virtual table `{table}` is outside the supported SQLite-compatible subset"
                )));
            }

            let columns = source_columns
                .get(&table.to_ascii_lowercase())
                .cloned()
                .ok_or_else(|| {
                    Error::Migration(format!("missing column metadata for `{table}`"))
                })?;
            if columns.is_empty() {
                return Err(Error::Migration(format!(
                    "table `{table}` has no transferable columns"
                )));
            }

            if destination_names.contains(&table.to_ascii_lowercase()) {
                if let Some(destination_columns) =
                    destination_columns.get(&table.to_ascii_lowercase())
                    && columns != *destination_columns
                {
                    return Err(Error::Migration(format!(
                        "table `{table}` has incompatible column definitions in the destination"
                    )));
                }
            } else {
                destination_txn.execute_batch(&create_sql).await?;
            }

            let quoted_table = Self::quote_identifier(&table);
            let quoted_columns = columns
                .iter()
                .map(|(name, _, _, _, _)| Self::quote_identifier(name))
                .collect::<Vec<_>>();
            let select_sql = format!("SELECT {} FROM {quoted_table}", quoted_columns.join(", "));
            let insert_sql = format!(
                "INSERT OR REPLACE INTO {quoted_table} ({}) VALUES ({})",
                quoted_columns.join(", "),
                (1..=columns.len())
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut rows = source_txn.query(select_sql, ()).await?;
            while let Some(row) = rows.next().await? {
                let values = (0..columns.len())
                    .map(|index| row.get_value(index))
                    .collect::<turso::Result<Vec<_>>>()?;
                destination_txn.execute(&insert_sql, values).await?;
            }
        }

        for (index, table, create_sql) in source_indexes {
            match destination_indexes.get(&index.to_ascii_lowercase()) {
                Some(existing) if existing != &create_sql => {
                    return Err(Error::Migration(format!(
                        "index `{index}` on `{table}` has an incompatible definition in the destination"
                    )));
                }
                Some(_) => {}
                None => destination_txn.execute_batch(&create_sql).await?,
            }
        }

        destination_txn.commit().await?;
        source_txn.commit().await?;
        drop(destination_conn);
        destination.push().await?;
        Ok(())
    }

    async fn user_tables(conn: &Connection) -> Result<Vec<(String, String)>, Error> {
        let mut rows = conn
            .query(
                "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND sql IS NOT NULL AND lower(substr(name, 1, 7)) != 'sqlite_' AND lower(substr(name, 1, 6)) != 'turso_' AND lower(substr(name, 1, 8)) != '__turso_' ORDER BY name",
                (),
            )
            .await?;
        let mut tables = Vec::new();
        while let Some(row) = rows.next().await? {
            tables.push((row.get(0)?, row.get(1)?));
        }
        Ok(tables)
    }

    /// Return the managed tables that exist in the database, in a stable order.
    async fn existing_managed_tables(conn: &Connection) -> Result<Vec<&'static str>, Error> {
        let mut rows = conn
            .query(
                "SELECT lower(name) FROM sqlite_master WHERE type = 'table' AND sql IS NOT NULL",
                (),
            )
            .await?;
        let mut present = std::collections::HashSet::new();
        while let Some(row) = rows.next().await? {
            present.insert(row.get::<String>(0)?);
        }
        Ok(MANAGED_TABLES
            .iter()
            .copied()
            .filter(|table| present.contains(*table))
            .collect())
    }

    async fn user_indexes(conn: &Connection) -> Result<Vec<(String, String, String)>, Error> {
        let mut rows = conn
            .query(
                "SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL ORDER BY name",
                (),
            )
            .await?;
        let mut indexes = Vec::new();
        while let Some(row) = rows.next().await? {
            indexes.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        Ok(indexes)
    }

    async fn table_columns(conn: &Connection, table: &str) -> Result<Vec<TableColumn>, Error> {
        let mut rows = conn
            .query(
                format!("PRAGMA table_info({})", Self::quote_identifier(table)),
                (),
            )
            .await?;
        let mut columns = Vec::new();
        while let Some(row) = rows.next().await? {
            columns.push((
                row.get(1)?,
                row.get(2)?,
                row.get::<i64>(3)?,
                row.get_value(4)?,
                row.get::<i64>(5)?,
            ));
        }
        Ok(columns)
    }

    fn quote_identifier(identifier: &str) -> String {
        format!("\"{}\"", identifier.replace('"', "\"\""))
    }

    /// Persist transaction-graph changes in a transaction owned by this store.
    ///
    /// Transactions, timestamps, outputs, and anchors are written together. For an atomic write of
    /// the complete wallet state, use the wallet changeset persister instead.
    pub async fn write_tx_graph(
        &self,
        tx_graph: &tx_graph::ChangeSet<ConfirmationBlockTime>,
    ) -> Result<(), Error> {
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        Self::write_tx_graph_in_transaction(&txn, tx_graph).await?;
        txn.commit().await?;
        drop(conn);
        self.push().await?;
        Ok(())
    }

    pub(crate) async fn write_tx_graph_in_transaction(
        conn: &DatabaseTransaction<'_>,
        tx_graph: &tx_graph::ChangeSet<ConfirmationBlockTime>,
    ) -> Result<(), Error> {
        for tx in &tx_graph.txs {
            let txid = tx.compute_txid();
            conn.execute(
                "INSERT INTO tx(txid, tx) VALUES(?1, ?2) ON CONFLICT DO UPDATE SET tx = ?2",
                (txid.to_string(), consensus::encode::serialize(tx)),
            )
            .await?;
        }
        for (txid, time) in &tx_graph.first_seen {
            conn.execute(
                "INSERT INTO tx(txid, first_seen) VALUES(?1, ?2) ON CONFLICT DO UPDATE SET first_seen = ?2",
                (txid.to_string(), i64::try_from(*time)?),
            )
            .await?;
        }
        for (txid, time) in &tx_graph.last_seen {
            conn.execute(
                "INSERT INTO tx(txid, last_seen) VALUES(?1, ?2) ON CONFLICT DO UPDATE SET last_seen = ?2",
                (txid.to_string(), i64::try_from(*time)?),
            )
            .await?;
        }
        for (txid, time) in &tx_graph.last_evicted {
            conn.execute(
                "INSERT INTO tx(txid, last_evicted) VALUES(?1, ?2) ON CONFLICT DO UPDATE SET last_evicted = ?2",
                (txid.to_string(), i64::try_from(*time)?),
            )
            .await?;
        }
        for (outpoint, txout) in &tx_graph.txouts {
            let OutPoint { txid, vout } = outpoint;
            let TxOut {
                value,
                script_pubkey,
            } = txout;
            conn.execute(
                "INSERT INTO txout(txid, vout, value, script) VALUES(?1, ?2, ?3, ?4) ON CONFLICT DO UPDATE SET value = ?3, script = ?4",
                (
                    txid.to_string(),
                    i64::from(*vout),
                    i64::try_from(value.to_sat())?,
                    script_pubkey.to_bytes(),
                ),
            )
            .await?;
        }
        for (anchor, txid) in &tx_graph.anchors {
            let BlockId { height, hash } = anchor.block_id;
            conn.execute(
                "INSERT INTO anchor(block_height, block_hash, txid, confirmation_time) VALUES(?1, ?2, ?3, ?4) ON CONFLICT DO UPDATE SET confirmation_time = ?4",
                (
                    i64::from(height),
                    hash.to_string(),
                    txid.to_string(),
                    i64::try_from(anchor.confirmation_time)?,
                ),
            )
            .await?;
        }

        Ok(())
    }

    /// Apply local-chain block changes in a transaction owned by this store.
    ///
    /// A block with a hash is inserted at its height if that height is not already present; a block
    /// with no hash removes the row at that height.
    pub async fn write_local_chain(
        &self,
        local_chain: &local_chain::ChangeSet,
    ) -> Result<(), Error> {
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        Self::write_local_chain_in_transaction(&txn, local_chain).await?;
        txn.commit().await?;
        drop(conn);
        self.push().await?;
        Ok(())
    }

    pub(crate) async fn write_local_chain_in_transaction(
        conn: &DatabaseTransaction<'_>,
        local_chain: &local_chain::ChangeSet,
    ) -> Result<(), Error> {
        for (&height, hash) in &local_chain.blocks {
            match hash {
                Some(hash) => {
                    conn.execute(
                        "INSERT INTO block(height, hash) VALUES(?1, ?2) ON CONFLICT(height) DO UPDATE SET hash = ?2",
                        (i64::from(height), hash.to_string()),
                    )
                    .await?;
                }
                None => {
                    conn.execute("DELETE FROM block WHERE height = ?1", [i64::from(height)])
                        .await?;
                }
            }
        }

        Ok(())
    }

    /// Persist keychain reveal indices and cached scripts in a store-owned transaction.
    ///
    /// Reveal indices and cached scripts are updated when present in the changeset.
    pub async fn write_keychain_txout(
        &self,
        keychain_txout: &keychain_txout::ChangeSet,
    ) -> Result<(), Error> {
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        Self::write_keychain_txout_in_transaction(&txn, keychain_txout).await?;
        txn.commit().await?;
        drop(conn);
        self.push().await?;
        Ok(())
    }

    pub(crate) async fn write_keychain_txout_in_transaction(
        conn: &DatabaseTransaction<'_>,
        keychain_txout: &keychain_txout::ChangeSet,
    ) -> Result<(), Error> {
        for (descriptor_id, last_revealed) in &keychain_txout.last_revealed {
            conn.execute(
                "INSERT INTO keychain_last_revealed(descriptor_id, last_revealed) VALUES(?1, ?2) ON CONFLICT DO UPDATE SET last_revealed = ?2",
                (descriptor_id.to_string(), i64::from(*last_revealed)),
            )
            .await?;
        }
        for (descriptor_id, spk_cache) in &keychain_txout.spk_cache {
            for (derivation_index, script) in spk_cache {
                conn.execute(
                    "INSERT INTO keychain_script_pubkey(descriptor_id, derivation_index, script) VALUES(?1, ?2, ?3) ON CONFLICT(descriptor_id, derivation_index) DO UPDATE SET script = ?3",
                    (
                        descriptor_id.to_string(),
                        i64::from(*derivation_index),
                        script.to_bytes(),
                    ),
                )
                .await?;
            }
        }

        Ok(())
    }

    /// Read transaction-graph changes through a transaction owned by this store.
    ///
    /// Returns the persisted transactions, timestamps, outputs, and confirmation anchors.
    pub async fn read_tx_graph(&self) -> Result<tx_graph::ChangeSet<ConfirmationBlockTime>, Error> {
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        let changeset = Self::read_tx_graph_in_transaction(&txn).await?;
        txn.commit().await?;
        Ok(changeset)
    }

    pub(crate) async fn read_tx_graph_in_transaction(
        conn: &DatabaseTransaction<'_>,
    ) -> Result<tx_graph::ChangeSet<ConfirmationBlockTime>, Error> {
        let mut changeset = tx_graph::ChangeSet::default();

        let mut rows = conn
            .query(
                "SELECT txid, tx, first_seen, last_seen, last_evicted FROM tx",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let txid: Txid = row.get::<String>(0)?.parse()?;
            let tx: Option<Vec<u8>> = row.get(1)?;
            let first_seen: Option<i64> = row.get(2)?;
            let last_seen: Option<i64> = row.get(3)?;
            let last_evicted: Option<i64> = row.get(4)?;
            if let Some(data) = tx {
                let tx: Transaction = consensus::encode::deserialize(&data)?;
                changeset.txs.insert(Arc::new(tx));
            }
            if let Some(first_seen) = first_seen {
                changeset.first_seen.insert(txid, first_seen.try_into()?);
            }
            if let Some(last_seen) = last_seen {
                changeset.last_seen.insert(txid, last_seen.try_into()?);
            }
            if let Some(last_evicted) = last_evicted {
                changeset
                    .last_evicted
                    .insert(txid, last_evicted.try_into()?);
            }
        }

        let mut rows = conn
            .query("SELECT txid, vout, value, script FROM txout", ())
            .await?;
        while let Some(row) = rows.next().await? {
            let txid: Txid = row.get::<String>(0)?.parse()?;
            let vout = u32::try_from(row.get::<i64>(1)?)?;
            let value = Amount::from_sat(u64::try_from(row.get::<i64>(2)?)?);
            let script_pubkey = ScriptBuf::from_bytes(row.get::<Vec<u8>>(3)?);
            changeset.txouts.insert(
                OutPoint { txid, vout },
                TxOut {
                    value,
                    script_pubkey,
                },
            );
        }

        let mut rows = conn
            .query(
                "SELECT block_height, block_hash, txid, confirmation_time FROM anchor",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let height = u32::try_from(row.get::<i64>(0)?)?;
            let hash: BlockHash = row.get::<String>(1)?.parse()?;
            let txid: Txid = row.get::<String>(2)?.parse()?;
            let confirmation_time = u64::try_from(row.get::<i64>(3)?)?;
            changeset.anchors.insert((
                ConfirmationBlockTime {
                    block_id: BlockId { height, hash },
                    confirmation_time,
                },
                txid,
            ));
        }

        Ok(changeset)
    }

    /// Read persisted local-chain blocks through a transaction owned by this store.
    ///
    /// Each stored height and hash is returned as a block entry with a hash.
    pub async fn read_local_chain(&self) -> Result<local_chain::ChangeSet, Error> {
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        let changeset = Self::read_local_chain_in_transaction(&txn).await?;
        txn.commit().await?;
        Ok(changeset)
    }

    pub(crate) async fn read_local_chain_in_transaction(
        conn: &DatabaseTransaction<'_>,
    ) -> Result<local_chain::ChangeSet, Error> {
        let mut changeset = local_chain::ChangeSet::default();
        let mut rows = conn.query("SELECT height, hash FROM block", ()).await?;
        while let Some(row) = rows.next().await? {
            let height = u32::try_from(row.get::<i64>(0)?)?;
            let hash: BlockHash = row.get::<String>(1)?.parse()?;
            changeset.blocks.insert(height, Some(hash));
        }

        Ok(changeset)
    }

    /// Read persisted reveal indices and cached scripts through a store-owned transaction.
    ///
    /// The returned changeset includes entries present in the database; an empty database yields
    /// empty maps.
    pub async fn read_keychain_txout(&self) -> Result<keychain_txout::ChangeSet, Error> {
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        let changeset = Self::read_keychain_txout_in_transaction(&txn).await?;
        txn.commit().await?;
        Ok(changeset)
    }

    pub(crate) async fn read_keychain_txout_in_transaction(
        conn: &DatabaseTransaction<'_>,
    ) -> Result<keychain_txout::ChangeSet, Error> {
        let mut changeset = keychain_txout::ChangeSet::default();

        let mut rows = conn
            .query(
                "SELECT descriptor_id, last_revealed FROM keychain_last_revealed",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let descriptor_id: DescriptorId = row.get::<String>(0)?.parse()?;
            let last_revealed = u32::try_from(row.get::<i64>(1)?)?;
            changeset.last_revealed.insert(descriptor_id, last_revealed);
        }

        let mut rows = conn
            .query(
                "SELECT descriptor_id, derivation_index, script FROM keychain_script_pubkey",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let descriptor_id: DescriptorId = row.get::<String>(0)?.parse()?;
            let derivation_index = u32::try_from(row.get::<i64>(1)?)?;
            let script = ScriptBuf::from_bytes(row.get::<Vec<u8>>(2)?);
            changeset
                .spk_cache
                .entry(descriptor_id)
                .or_default()
                .insert(derivation_index, script);
        }

        Ok(changeset)
    }

    /// Store a PSBT under the txid of its unsigned transaction.
    ///
    /// Writing another PSBT with the same unsigned transaction ID replaces the previous one. The
    /// complete PSBT, including signing metadata, is serialized. Run [`Store::migrate`] first.
    ///
    /// PSBTs can contain partial signatures, so persistence requires a database opened with local
    /// at-rest encryption. Remote sync stores are an exception: their local cache is not encrypted
    /// by Turso Cloud settings, so PSBT persistence is permitted there without local encryption.
    /// Stores created without `Options::with_encryption`, stores created by
    /// [`Store::from_database`] whose opening options are unknown, and local stores without
    /// encryption return [`Error::PsbtRequiresEncryption`].
    #[cfg(feature = "psbt")]
    pub async fn write_psbt(&self, psbt: &bitcoin::Psbt) -> Result<bitcoin::Txid, Error> {
        self.require_encryption()?;
        let txid = psbt.unsigned_tx.compute_txid();
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        txn.execute(
            "INSERT INTO psbt(unsigned_txid, data) VALUES(?1, ?2) ON CONFLICT(unsigned_txid) DO UPDATE SET data = ?2",
            (txid.to_string(), psbt.serialize()),
        )
        .await?;
        txn.commit().await?;
        drop(conn);
        self.push().await?;
        Ok(txid)
    }

    /// Load the PSBT stored for an unsigned transaction ID.
    ///
    /// Returns `Ok(None)` when no PSBT is stored for `txid`. This method opens and commits its own
    /// read transaction. Run [`Store::migrate`] first. Like [`Store::write_psbt`], this requires a
    /// store opened with local at-rest encryption, except for remote sync stores whose local cache
    /// is not encrypted by Turso Cloud settings.
    #[cfg(feature = "psbt")]
    pub async fn read_psbt(&self, txid: bitcoin::Txid) -> Result<Option<bitcoin::Psbt>, Error> {
        self.require_encryption()?;
        let mut conn = self.connect().await?;
        let txn = conn.transaction().await?;
        let mut rows = txn
            .query(
                "SELECT data FROM psbt WHERE unsigned_txid = ?1",
                [txid.to_string()],
            )
            .await?;
        let data = rows
            .next()
            .await?
            .map(|row| row.get::<Vec<u8>>(0))
            .transpose()?;
        let psbt = data
            .map(|data| bitcoin::Psbt::deserialize(&data))
            .transpose()?;
        txn.commit().await?;
        Ok(psbt)
    }

    /// PSBT persistence requires local at-rest encryption, except for remote sync stores whose
    /// local cache is not encrypted by Turso Cloud settings.
    #[cfg(feature = "psbt")]
    fn require_encryption(&self) -> Result<(), Error> {
        #[cfg(feature = "remote")]
        {
            if matches!(&self.database, StoreDatabase::Remote(_)) {
                return Ok(());
            }
        }
        match self.options.as_ref() {
            Some(options) if options.encryption.is_some() => Ok(()),
            _ => Err(Error::PsbtRequiresEncryption),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use bitcoin::hashes::Hash;

    #[test]
    fn options_default_to_disabled() {
        let options = Options::new();

        assert!(options.busy_timeout.is_none());
        assert!(options.transaction_behavior.is_none());
        assert!(options.encryption.is_none());
        #[cfg(feature = "remote")]
        {
            assert!(options.remote_encryption.is_none());
            assert!(options.bootstrap_if_empty.is_none());
            assert!(options.long_poll_timeout.is_none());
            assert!(options.client_name.is_none());
            assert!(options.auth_token_fn.is_none());
            assert!(options.partial_sync_opts.is_none());
            assert!(options.logical_mvcc_pull.is_none());
        }
    }

    #[test]
    fn busy_timeout_records_the_configured_value() {
        let default = Options::new();
        assert_eq!(default.busy_timeout, None);

        let configured = Options::new().with_busy_timeout(Duration::from_millis(250));
        assert_eq!(configured.busy_timeout, Some(Duration::from_millis(250)));
    }

    #[tokio::test]
    async fn store_defaults_to_a_five_second_busy_timeout() -> anyhow::Result<()> {
        assert_eq!(DEFAULT_BUSY_TIMEOUT, Duration::from_secs(5));

        let store = Store::new_memory().await?;
        assert_eq!(store.configured_busy_timeout(), Duration::from_secs(5));

        let store = Store::new_with_options(
            ":memory:",
            Options::new().with_busy_timeout(Duration::from_millis(250)),
        )
        .await?;
        assert_eq!(store.configured_busy_timeout(), Duration::from_millis(250));
        Ok(())
    }

    #[test]
    fn generated_encryption_opts_use_a_random_aes256gcm_key() {
        let first = generate_encryption_opts();
        let second = generate_encryption_opts();

        assert_eq!(first.cipher, "aes256gcm");
        assert_eq!(first.hexkey.len(), 64);
        assert!(first.hexkey.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first.hexkey, second.hexkey);
    }

    #[cfg(feature = "remote")]
    #[tokio::test]
    async fn unsupported_remote_options_return_actionable_errors() -> anyhow::Result<()> {
        let local_encryption = Options::new().with_encryption(EncryptionOpts {
            cipher: "aes256gcm".to_owned(),
            hexkey: "00".repeat(32),
        });
        let error = Store::new_remote_with_options(
            ":memory:",
            "libsql://unused.turso.io",
            "unused-token",
            local_encryption,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, Error::UnsupportedOption(message) if message.contains("local at-rest encryption"))
        );

        let error = Store::new_remote_with_options(
            ":memory:",
            "libsql://unused.turso.io",
            "unused-token",
            Options::new().with_transaction_behavior(TransactionBehavior::Concurrent),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, Error::UnsupportedOption(message) if message.contains("Concurrent transaction behavior"))
        );

        let remote_encryption = Options::new()
            .with_remote_encryption("unused-key", turso::sync::RemoteEncryptionCipher::Aes256Gcm);
        let error = Store::new_with_options(":memory:", remote_encryption)
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::UnsupportedOption(message) if message.contains("remote encryption"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_stores_have_no_remote_cache_surface() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        assert!(!store.pull().await?);
        store.checkpoint().await?;
        store.flush_cache().await?;
        assert!(store.cache_stats().await?.is_none());
        assert!(store.cache_path().is_none());
        Ok(())
    }

    #[cfg(feature = "remote")]
    #[tokio::test]
    async fn remote_cache_options_are_recorded() {
        let partial = PartialSyncOpts {
            bootstrap_strategy: None,
            segment_size: 4096,
            prefetch: true,
        };
        let options = Options::new()
            .with_bootstrap_if_empty(false)
            .with_long_poll_timeout(Duration::from_secs(5))
            .with_client_name("bdk-turso-test")
            .with_partial_sync_opts_experimental(partial)
            .with_logical_mvcc_pull(true);

        assert_eq!(options.bootstrap_if_empty, Some(false));
        assert_eq!(options.long_poll_timeout, Some(Duration::from_secs(5)));
        assert_eq!(options.client_name.as_deref(), Some("bdk-turso-test"));
        assert!(options.partial_sync_opts.is_some());
        assert_eq!(options.logical_mvcc_pull, Some(true));
        assert!(options.has_sync_cache_options());
    }

    #[cfg(feature = "remote")]
    #[tokio::test]
    async fn auth_token_fn_is_recorded_and_callable() {
        let options = Options::new()
            .with_auth_token_fn(|| async { Ok::<_, turso::Error>("rotated-token".to_owned()) });
        let token_fn = options
            .auth_token_fn
            .as_ref()
            .expect("auth token callback is recorded");
        assert_eq!(
            token_fn().await.expect("token callback succeeds"),
            "rotated-token"
        );
    }

    #[cfg(feature = "remote")]
    #[tokio::test]
    async fn remote_cache_options_are_rejected_by_local_stores() -> anyhow::Result<()> {
        let error = Store::new_with_options(":memory:", Options::new().with_client_name("local"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::UnsupportedOption(message) if message.contains("remote sync cache options"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn wrapped_database_options_are_unknown() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        assert!(store.options().is_some());

        let database = Builder::new_local(":memory:").build().await?;
        let wrapped = Store::from_database(database);
        assert!(wrapped.options().is_none());
        Ok(())
    }

    #[tokio::test]
    async fn migrate_creates_default_and_feature_enabled_schemas() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        store.migrate().await?;

        let conn = store.connect().await?;
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'block'",
                (),
            )
            .await?;
        assert!(rows.next().await?.is_some());

        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'network'",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.is_some(), cfg!(feature = "wallet"));

        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'psbt'",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.is_some(), cfg!(feature = "psbt"));
        Ok(())
    }

    #[tokio::test]
    async fn migrate_creates_lookup_indexes() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        store.migrate().await?;

        let names = index_names(&store).await?;
        for expected in [
            "anchor_block_hash_idx",
            "anchor_txid_idx",
            "block_hash_idx",
            "txout_script_idx",
        ] {
            assert!(
                names.contains(&expected.to_owned()),
                "missing index `{expected}`; found {names:?}"
            );
        }
        #[cfg(feature = "wallet")]
        assert!(
            names.contains(&"keychain_script_pubkey_script_idx".to_owned()),
            "missing wallet script index; found {names:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn migrate_to_copies_indexes() -> anyhow::Result<()> {
        let source = Store::new_memory().await?;
        source.migrate().await?;

        let destination = Store::new_memory().await?;
        source.migrate_to(&destination).await?;

        assert_eq!(
            index_names(&source).await?,
            index_names(&destination).await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn wipe_clears_managed_tables_created_by_other_builds() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        let conn = store.connect().await?;
        conn.execute_batch(
            "CREATE TABLE block (height INTEGER PRIMARY KEY NOT NULL, hash TEXT NOT NULL);
             CREATE TABLE tx (txid TEXT NOT NULL PRIMARY KEY);
             CREATE TABLE txout (
                 txid TEXT NOT NULL,
                 vout INTEGER NOT NULL,
                 PRIMARY KEY (txid, vout)
             );
             CREATE TABLE anchor (
                 block_height INTEGER NOT NULL,
                 block_hash TEXT NOT NULL,
                 txid TEXT NOT NULL,
                 confirmation_time INTEGER NOT NULL,
                 PRIMARY KEY (block_height, block_hash, txid)
             );
             CREATE TABLE keychain (keychain INTEGER NOT NULL PRIMARY KEY, descriptor TEXT NOT NULL);
             CREATE TABLE keychain_last_revealed (
                 descriptor_id TEXT NOT NULL PRIMARY KEY,
                 last_revealed INTEGER
             );
             CREATE TABLE keychain_script_pubkey (
                 descriptor_id TEXT NOT NULL,
                 derivation_index INTEGER,
                 script BLOB,
                 PRIMARY KEY (descriptor_id, derivation_index)
             );
             CREATE TABLE network (
                 id INTEGER NOT NULL DEFAULT 0 CHECK (id = 0),
                 network TEXT NOT NULL,
                 PRIMARY KEY (id)
             );
             CREATE TABLE locked_outpoint (
                 txid TEXT NOT NULL,
                 vout INTEGER NOT NULL,
                 PRIMARY KEY (txid, vout)
             );
             CREATE TABLE psbt (unsigned_txid TEXT NOT NULL PRIMARY KEY, data BLOB NOT NULL);
             INSERT INTO block VALUES (1, 'hash');
             INSERT INTO tx (txid) VALUES ('tx');
             INSERT INTO txout (txid, vout) VALUES ('tx', 0);
             INSERT INTO anchor (block_height, block_hash, txid, confirmation_time)
                 VALUES (1, 'hash', 'tx', 0);
             INSERT INTO keychain VALUES (0, 'descriptor');
             INSERT INTO keychain_last_revealed VALUES ('descriptor', 0);
             INSERT INTO keychain_script_pubkey (descriptor_id, derivation_index, script)
                 VALUES ('descriptor', 0, x'00');
             INSERT INTO network (id, network) VALUES (0, 'signet');
             INSERT INTO locked_outpoint VALUES ('tx', 0);
             INSERT INTO psbt VALUES ('txid', x'00');",
        )
        .await?;
        drop(conn);

        store.wipe().await?;

        let conn = store.connect().await?;
        for table in MANAGED_TABLES {
            let mut rows = conn
                .query(
                    format!("SELECT COUNT(*) FROM {}", Store::quote_identifier(table)),
                    (),
                )
                .await?;
            let row = rows.next().await?.expect("COUNT returns one row");
            assert_eq!(row.get::<i64>(0)?, 0, "table `{table}` was not wiped");
        }
        Ok(())
    }

    #[tokio::test]
    async fn wipe_leaves_unmanaged_tables_untouched() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        let conn = store.connect().await?;
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings VALUES ('theme', 'dark');
             CREATE TABLE block (height INTEGER PRIMARY KEY NOT NULL, hash TEXT NOT NULL);
             INSERT INTO block VALUES (1, 'hash');",
        )
        .await?;
        drop(conn);

        store.wipe().await?;

        let conn = store.connect().await?;
        let mut rows = conn.query("SELECT COUNT(*) FROM app_settings", ()).await?;
        let row = rows.next().await?.expect("COUNT returns one row");
        assert_eq!(row.get::<i64>(0)?, 1, "unmanaged rows were deleted");
        Ok(())
    }

    async fn index_names(store: &Store) -> anyhow::Result<Vec<String>> {
        let conn = store.connect().await?;
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL ORDER BY name",
                (),
            )
            .await?;
        let mut names = Vec::new();
        while let Some(row) = rows.next().await? {
            names.push(row.get::<String>(0)?);
        }
        Ok(names)
    }

    #[test]
    fn transaction_behavior_last_call_wins() {
        let options = Options::new()
            .with_transaction_behavior(TransactionBehavior::Immediate)
            .with_transaction_behavior(TransactionBehavior::Exclusive);

        assert!(matches!(
            options.transaction_behavior,
            Some(TransactionBehavior::Exclusive)
        ));
    }

    #[tokio::test]
    async fn concurrent_transaction_behavior_enables_mvcc() -> anyhow::Result<()> {
        let store = Store::new_memory_with_options(
            Options::new().with_transaction_behavior(TransactionBehavior::Concurrent),
        )
        .await?;
        store.migrate().await?;

        let conn = store.connect().await?;
        let mut rows = conn.query("PRAGMA journal_mode", ()).await?;
        let row = rows
            .next()
            .await?
            .expect("journal_mode should return a row");
        assert_eq!(row.get::<String>(0)?, "mvcc");

        let mut changeset = local_chain::ChangeSet::default();
        changeset.blocks.insert(7, Some(Hash::hash(b"concurrent")));
        store.write_local_chain(&changeset).await?;
        assert_eq!(store.read_local_chain().await?, changeset);
        Ok(())
    }

    #[tokio::test]
    async fn each_transaction_behavior_can_write() -> anyhow::Result<()> {
        for behavior in [
            TransactionBehavior::Deferred,
            TransactionBehavior::Immediate,
            TransactionBehavior::Exclusive,
        ] {
            let store =
                Store::new_memory_with_options(Options::new().with_transaction_behavior(behavior))
                    .await?;
            store.migrate().await?;

            let mut changeset = local_chain::ChangeSet::default();
            changeset.blocks.insert(1, Some(Hash::hash(b"behavior")));
            store.write_local_chain(&changeset).await?;
            assert_eq!(store.read_local_chain().await?, changeset);
        }
        Ok(())
    }

    #[tokio::test]
    async fn lookup_queries_use_default_indexes() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        store.migrate().await?;
        let conn = store.connect().await?;

        let mut lookups = vec![
            (
                "EXPLAIN QUERY PLAN SELECT height FROM block WHERE hash = 'x'",
                "block_hash_idx",
            ),
            (
                "EXPLAIN QUERY PLAN SELECT txid FROM txout WHERE script = x'00'",
                "txout_script_idx",
            ),
            (
                "EXPLAIN QUERY PLAN SELECT block_height FROM anchor WHERE txid = 'x'",
                "anchor_txid_idx",
            ),
            (
                "EXPLAIN QUERY PLAN SELECT txid FROM anchor WHERE block_hash = 'x'",
                "anchor_block_hash_idx",
            ),
        ];
        #[cfg(feature = "wallet")]
        lookups.push((
            "EXPLAIN QUERY PLAN SELECT descriptor_id FROM keychain_script_pubkey WHERE script = x'00'",
            "keychain_script_pubkey_script_idx",
        ));

        for (sql, index) in lookups {
            let mut rows = conn.query(sql, ()).await?;
            let mut plan = String::new();
            while let Some(row) = rows.next().await? {
                plan.push_str(&row.get::<String>(3)?);
                plan.push('\n');
            }
            assert!(
                plan.contains(index),
                "`{sql}` did not use `{index}`:\n{plan}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn block_table_height_rebases_reorgs() -> anyhow::Result<()> {
        let mut changeset = local_chain::ChangeSet::default();
        changeset.blocks.insert(0, Some(Hash::hash(b"0")));
        changeset.blocks.insert(1, Some(Hash::hash(b"1")));

        let store = Store::new_memory().await?;
        store.migrate().await?;
        store.write_local_chain(&changeset).await?;

        changeset.blocks.insert(1, Some(Hash::hash(b"1a")));
        store.write_local_chain(&changeset).await?;

        let conn = store.connect().await?;
        let mut rows = conn
            .query("SELECT height, hash FROM block WHERE height = 1", ())
            .await?;
        let row = rows.next().await?.expect("expected block row");
        let row_hash: String = row.get(1)?;
        let expected_hash = BlockHash::hash(b"1a").to_string();
        assert_eq!(row_hash, expected_hash);

        let mut changeset = local_chain::ChangeSet::default();
        changeset.blocks.insert(1, None);
        store.write_local_chain(&changeset).await?;

        changeset.blocks.insert(1, Some(Hash::hash(b"1a")));
        store.write_local_chain(&changeset).await?;

        let conn = store.connect().await?;
        let mut rows = conn
            .query("SELECT height, hash FROM block WHERE height = 1", ())
            .await?;
        let row = rows.next().await?.expect("expected block row");
        let row_hash: String = row.get(1)?;
        let expected_hash = BlockHash::hash(b"1a").to_string();
        assert_eq!(row_hash, expected_hash);

        Ok(())
    }
}
