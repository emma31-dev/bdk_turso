//! [`Store`] provides async read and write methods for persisting BDK change sets with Turso.

use std::{fmt, sync::Arc};

use bdk_chain::{
    BlockId, ConfirmationBlockTime, DescriptorId, bitcoin, keychain_txout, local_chain, tx_graph,
};
use bitcoin::{Amount, BlockHash, OutPoint, ScriptBuf, Transaction, TxOut, Txid, consensus};
pub use turso::EncryptionOpts;
use turso::{Builder, Connection, Database, transaction::Transaction as DatabaseTransaction};

use crate::Error;

/// Experimental Turso features to enable when opening a store.
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
    /// Enables encryption and supplies its cipher and key.
    ///
    /// Encrypted database files are not readable by SQLite-compatible backends.
    encryption: Option<EncryptionOpts>,
}

impl Options {
    /// Create options with all experimental features disabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable experimental index methods.
    pub fn with_index_method(mut self) -> Self {
        self.index_method = true;
        self
    }

    /// Enable multi-process WAL.
    pub fn with_multiprocess_wal(mut self) -> Self {
        self.multiprocess_wal = true;
        self
    }

    /// Enable experimental `VACUUM` support.
    pub fn with_vacuum(mut self) -> Self {
        self.vacuum = true;
        self
    }

    /// Enable `ATTACH DATABASE` and `DETACH DATABASE`.
    pub fn with_attach(mut self) -> Self {
        self.attach = true;
        self
    }

    /// Enable generated columns.
    pub fn with_generated_columns(mut self) -> Self {
        self.generated_columns = true;
        self
    }

    /// Enable `WITHOUT ROWID` tables.
    pub fn with_without_rowid(mut self) -> Self {
        self.without_rowid = true;
        self
    }

    /// Enable MVCC passive checkpointing.
    pub fn with_mvcc_passive_checkpoint(mut self) -> Self {
        self.mvcc_passive_checkpoint = true;
        self
    }

    /// Enable encryption with the provided cipher and key.
    pub fn with_encryption(mut self, encryption: EncryptionOpts) -> Self {
        self.encryption = Some(encryption);
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
            .finish()
    }
}

/// Store.
#[derive(Debug, Clone)]
pub struct Store {
    pub(crate) database: Database,
    options: Options,
}

impl Store {
    /// Create a new in-memory store.
    pub async fn new_memory() -> Result<Self, Error> {
        Self::new_memory_with_options(Options::default()).await
    }

    /// Create an in-memory store with experimental features enabled.
    pub async fn new_memory_with_options(options: Options) -> Result<Self, Error> {
        Self::new_with_options(":memory:", options).await
    }

    /// Create or open a store at the given local database path.
    pub async fn new(path: &str) -> Result<Self, Error> {
        Self::new_with_options(path, Options::default()).await
    }

    /// Create or open a store with experimental features enabled.
    pub async fn new_with_options(path: &str, options: Options) -> Result<Self, Error> {
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
        Ok(Self { database, options })
    }

    /// Create a store from an existing Turso database.
    pub fn from_database(database: Database) -> Self {
        Self {
            database,
            options: Options::default(),
        }
    }

    /// Returns the options used to open this store.
    pub fn options(&self) -> &Options {
        &self.options
    }

    pub(crate) fn connect(&self) -> Result<Connection, Error> {
        Ok(self.database.connect()?)
    }

    /// Runs the schema migration against the database.
    pub async fn migrate(&self) -> Result<(), Error> {
        let mut conn = self.connect()?;
        let txn = conn.transaction().await?;
        txn.execute_batch(include_str!("../migrations/0001_schema.up.sql"))
            .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Write tx_graph.
    pub async fn write_tx_graph(
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
                "INSERT OR IGNORE INTO anchor(block_height, block_hash, txid, confirmation_time) VALUES(?1, ?2, ?3, ?4)",
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

    /// Write local_chain.
    pub async fn write_local_chain(
        conn: &DatabaseTransaction<'_>,
        local_chain: &local_chain::ChangeSet,
    ) -> Result<(), Error> {
        for (&height, hash) in &local_chain.blocks {
            match hash {
                Some(hash) => {
                    conn.execute(
                        "INSERT OR IGNORE INTO block(height, hash) VALUES(?1, ?2)",
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

    /// Write keychain_txout.
    pub async fn write_keychain_txout(
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
                    "INSERT OR IGNORE INTO keychain_script_pubkey(descriptor_id, derivation_index, script) VALUES(?1, ?2, ?3)",
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

    /// Read tx_graph.
    pub async fn read_tx_graph(
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

    /// Read local_chain.
    pub async fn read_local_chain(
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

    /// Read keychain_txout.
    pub async fn read_keychain_txout(
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
    }

    #[tokio::test]
    async fn block_table_height_is_unique() -> anyhow::Result<()> {
        let mut changeset = local_chain::ChangeSet::default();
        changeset.blocks.insert(0, Some(Hash::hash(b"0")));
        changeset.blocks.insert(1, Some(Hash::hash(b"1")));

        let store = Store::new_memory().await?;
        store.migrate().await?;

        let mut conn = store.connect()?;
        let txn = conn.transaction().await?;
        Store::write_local_chain(&txn, &changeset).await?;
        txn.commit().await?;

        changeset.blocks.insert(1, Some(Hash::hash(b"1a")));
        let mut conn = store.connect()?;
        let txn = conn.transaction().await?;
        Store::write_local_chain(&txn, &changeset).await?;
        txn.commit().await?;

        let conn = store.connect()?;
        let mut rows = conn
            .query("SELECT height, hash FROM block WHERE height = 1", ())
            .await?;
        let row = rows.next().await?.expect("expected block row");
        let row_hash: String = row.get(1)?;
        let expected_hash = BlockHash::hash(b"1").to_string();
        assert_eq!(row_hash, expected_hash);

        let mut changeset = local_chain::ChangeSet::default();
        changeset.blocks.insert(1, None);
        let mut conn = store.connect()?;
        let txn = conn.transaction().await?;
        Store::write_local_chain(&txn, &changeset).await?;
        txn.commit().await?;

        changeset.blocks.insert(1, Some(Hash::hash(b"1a")));
        let mut conn = store.connect()?;
        let txn = conn.transaction().await?;
        Store::write_local_chain(&txn, &changeset).await?;
        txn.commit().await?;

        let conn = store.connect()?;
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
