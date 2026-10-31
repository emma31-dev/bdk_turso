//! Async Turso storage for BDK wallet and chain changesets.
//!
//! [`Store`] owns a Turso database connection pool and exposes methods to migrate the schema and
//! read or write persisted changesets.

use std::{fmt, sync::Arc};

use bdk_chain::{
    BlockId, ConfirmationBlockTime, DescriptorId, bitcoin, keychain_txout, local_chain, tx_graph,
};
use bitcoin::{Amount, BlockHash, OutPoint, ScriptBuf, Transaction, TxOut, Txid, consensus};
pub use turso::EncryptionOpts;
#[cfg(feature = "remote")]
pub use turso::sync::RemoteEncryptionCipher;
use turso::{
    Builder, Connection, Database as LocalDatabase, transaction::Transaction as DatabaseTransaction,
};

use crate::Error;

/// Opt-in Turso features applied when opening a [`Store`].
///
/// All experimental features are disabled by default. Configure an `Options` value before opening
/// the database; these settings cannot be changed on an already-open database.
#[derive(Clone, Default)]
pub struct Options {
    /// Enables Turso's experimental index-method support.
    index_method: bool,
    /// Enables opening the database from multiple processes.
    multiprocess_wal: bool,
    /// Enables the experimental `VACUUM` command.
    vacuum: bool,
    /// Enables `ATTACH DATABASE` and `DETACH DATABASE`.
    attach: bool,
    /// Enables generated columns.
    generated_columns: bool,
    /// Enables `WITHOUT ROWID` tables.
    without_rowid: bool,
    /// Enables the MVCC passive checkpoint option.
    mvcc_passive_checkpoint: bool,
    /// Enables local at-rest encryption and supplies its cipher and key.
    encryption: Option<EncryptionOpts>,
    #[cfg(feature = "remote")]
    /// Enables Turso Cloud encryption; this does not encrypt the local sync cache.
    remote_encryption: Option<(String, RemoteEncryptionCipher)>,
}

impl Options {
    /// Create options with every experimental feature disabled.
    ///
    /// Enable only the capabilities needed by the application by chaining the relevant `with_*`
    /// methods before passing the options to a store constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable Turso's experimental index-method support for the opened database.
    ///
    /// This setting must be enabled before opening the database and only affects databases opened
    /// with the returned options.
    pub fn with_index_method(mut self) -> Self {
        self.index_method = true;
        self
    }

    /// Enable multi-process access to the database through Turso's WAL coordinator.
    ///
    /// Use this only when the same database file must be shared between operating-system
    /// processes; it is disabled by default.
    pub fn with_multiprocess_wal(mut self) -> Self {
        self.multiprocess_wal = true;
        self
    }

    /// Enable Turso's experimental `VACUUM` support for the opened database.
    ///
    /// The option makes the command available; it does not run vacuuming automatically.
    pub fn with_vacuum(mut self) -> Self {
        self.vacuum = true;
        self
    }

    /// Enable SQL `ATTACH DATABASE` and `DETACH DATABASE` statements.
    ///
    /// This is disabled by default and is only applied when the database is opened.
    pub fn with_attach(mut self) -> Self {
        self.attach = true;
        self
    }

    /// Enable Turso's experimental generated-column support for SQL schemas.
    ///
    /// This setting does not alter the crate's wallet schema; it allows applications to use
    /// generated columns in their own database operations.
    pub fn with_generated_columns(mut self) -> Self {
        self.generated_columns = true;
        self
    }

    /// Enable `WITHOUT ROWID` table definitions in the opened database.
    ///
    /// Enable this only if application SQL needs to create such tables.
    pub fn with_without_rowid(mut self) -> Self {
        self.without_rowid = true;
        self
    }

    /// Enable Turso's experimental passive-checkpoint behavior for MVCC databases.
    ///
    /// This setting is opt-in and does not initiate a checkpoint by itself.
    pub fn with_mvcc_passive_checkpoint(mut self) -> Self {
        self.mvcc_passive_checkpoint = true;
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
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("index_method", &self.index_method)
            .field("multiprocess_wal", &self.multiprocess_wal)
            .field("vacuum", &self.vacuum)
            .field("attach", &self.attach)
            .field("generated_columns", &self.generated_columns)
            .field("without_rowid", &self.without_rowid)
            .field("mvcc_passive_checkpoint", &self.mvcc_passive_checkpoint)
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
            .finish()
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
    /// Create an in-memory database with all experimental options disabled.
    ///
    /// The database exists for the lifetime of the returned store and is not persisted to a file.
    /// Use [`Store::new_memory_with_options`] when an in-memory database needs experimental
    /// features.
    pub async fn new_memory() -> Result<Self, Error> {
        Self::new_memory_with_options(Options::default()).await
    }

    /// Create an in-memory database using the supplied experimental options.
    ///
    /// The options are applied while Turso opens the database. The database remains in memory for
    /// the lifetime of the store and is not persisted to disk.
    pub async fn new_memory_with_options(options: Options) -> Result<Self, Error> {
        Self::new_with_options(":memory:", options).await
    }

    /// Create or open a local database using default options.
    ///
    /// This constructor leaves all experimental options disabled. It does not create the BDK
    /// schema; call [`Store::migrate`] before using the persistence methods, unless the store is
    /// initialized through `AsyncWalletPersister`.
    pub async fn new(path: &str) -> Result<Self, Error> {
        Self::new_with_options(path, Options::default()).await
    }

    /// Create or open a local database with the supplied Turso options.
    ///
    /// The options are applied only while opening the database. All experimental features remain
    /// disabled unless explicitly enabled on `options`. This constructor does not run the BDK
    /// schema migration; call [`Store::migrate`] before using the persistence methods.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use bdk_turso::{Options, Store};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let options = Options::new().with_attach().with_generated_columns();
    /// let store = Store::new_with_options("wallet.sqlite", options).await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn new_with_options(path: &str, options: Options) -> Result<Self, Error> {
        #[cfg(feature = "remote")]
        if options.remote_encryption.is_some() {
            return Err(Error::UnsupportedOption(
                "remote encryption can only be used with Store::new_remote_with_options".into(),
            ));
        }

        let mut builder = Builder::new_local(path)
            .experimental_index_method(options.index_method)
            .experimental_multiprocess_wal(options.multiprocess_wal)
            .experimental_vacuum(options.vacuum)
            .experimental_attach(options.attach)
            .experimental_generated_columns(options.generated_columns)
            .experimental_without_rowid(options.without_rowid)
            .experimental_mvcc_passive_checkpoint(options.mvcc_passive_checkpoint);

        if let Some(encryption) = &options.encryption {
            builder = builder
                .experimental_encryption(true)
                .with_encryption(encryption.clone());
        }

        let database = builder.build().await?;
        Ok(Self {
            database: StoreDatabase::Local(database),
            options: Some(options),
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
    /// Local at-rest encryption and MVCC passive checkpointing are unsupported in sync mode and
    /// return an error. Use [`Options::with_remote_encryption`] for Turso Cloud encryption; this
    /// does not encrypt the local cache. The remaining experimental engine options are forwarded to
    /// Turso's sync builder.
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
        if options.mvcc_passive_checkpoint {
            return Err(Error::UnsupportedOption(
                "MVCC passive checkpointing is not supported by Turso's remote sync builder".into(),
            ));
        }

        let mut builder = turso::sync::Builder::new_remote(local_path)
            .with_remote_url(remote_url)
            .with_auth_token(auth_token)
            .experimental_index_method(options.index_method)
            .experimental_multiprocess_wal(options.multiprocess_wal)
            .experimental_vacuum(options.vacuum)
            .experimental_attach(options.attach)
            .experimental_generated_columns(options.generated_columns)
            .experimental_without_rowid(options.without_rowid);

        if let Some((key, cipher)) = &options.remote_encryption {
            builder = builder.with_remote_encryption(key.clone(), *cipher);
        }

        let database = builder.build().await?;
        Ok(Self {
            database: StoreDatabase::Remote(database),
            options: Some(options),
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
        match &self.database {
            StoreDatabase::Local(database) => Ok(database.connect()?),
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => {
                database.pull().await?;
                Ok(database.connect().await?)
            }
        }
    }

    pub(crate) async fn push(&self) -> Result<(), Error> {
        match &self.database {
            StoreDatabase::Local(_) => Ok(()),
            #[cfg(feature = "remote")]
            StoreDatabase::Remote(database) => Ok(database.push().await?),
        }
    }

    /// Apply the default schema and schemas for enabled features in a transaction.
    ///
    /// Core chain and transaction tables are always created. Wallet-specific tables are created
    /// only with the `wallet` feature, and the PSBT table only with `psbt`. This migration is safe
    /// to run repeatedly; `AsyncWalletPersister::initialize` runs it for you when enabled.
    pub async fn migrate(&self) -> Result<(), Error> {
        let mut conn = self.connect().await?;
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
    #[cfg(feature = "psbt")]
    pub async fn write_psbt(&self, psbt: &bitcoin::Psbt) -> Result<bitcoin::Txid, Error> {
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
    /// read transaction. Run [`Store::migrate`] first.
    #[cfg(feature = "psbt")]
    pub async fn read_psbt(&self, txid: bitcoin::Txid) -> Result<Option<bitcoin::Psbt>, Error> {
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
}

#[cfg(test)]
mod test {
    use super::*;
    use bitcoin::hashes::Hash;

    #[test]
    fn options_default_to_disabled() {
        let options = Options::new();

        assert!(!options.index_method);
        assert!(!options.multiprocess_wal);
        assert!(!options.vacuum);
        assert!(!options.attach);
        assert!(!options.generated_columns);
        assert!(!options.without_rowid);
        assert!(!options.mvcc_passive_checkpoint);
        assert!(options.encryption.is_none());
        #[cfg(feature = "remote")]
        assert!(options.remote_encryption.is_none());
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
            Options::new().with_mvcc_passive_checkpoint(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, Error::UnsupportedOption(message) if message.contains("MVCC passive checkpointing"))
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
