//! [`AsyncWalletPersister`] implementation for [`Store`], plus helpers for reading and writing
//! the individual wallet changeset components.

use std::{collections::BTreeMap, future::Future, pin::Pin, str::FromStr};

use bdk_chain::bitcoin;
use bdk_chain::miniscript;
use bdk_wallet::{AsyncWalletPersister, ChangeSet, KeychainKind, locked_outpoints};
use bitcoin::{Network, OutPoint};
use miniscript::descriptor::{Descriptor, DescriptorPublicKey};
use turso::transaction::Transaction as DatabaseTransaction;

use crate::{Error, Store};

impl Store {
    /// Persist every component of a wallet changeset in one transaction.
    ///
    /// Network, descriptors, local-chain blocks, transaction-graph data, keychain scripts, and
    /// locked outpoints are committed together. Run [`Store::migrate`] first, or initialize the
    /// store through the async wallet persister, which migrates automatically.
    pub async fn write_changeset(&self, changeset: &ChangeSet) -> Result<(), Error> {
        let mut conn = self.connect()?;
        let txn = conn.transaction().await?;

        if let Some(network) = changeset.network {
            Self::write_network(&txn, network).await?;
        }

        let mut descriptors = BTreeMap::new();
        if let Some(ref descriptor) = changeset.descriptor {
            descriptors.insert(KeychainKind::External, descriptor.clone());
        }
        if let Some(ref change_descriptor) = changeset.change_descriptor {
            descriptors.insert(KeychainKind::Internal, change_descriptor.clone());
        }
        Self::write_keychain_descriptors(&txn, &descriptors).await?;

        Self::write_local_chain(&txn, &changeset.local_chain).await?;
        Self::write_tx_graph(&txn, &changeset.tx_graph).await?;
        Self::write_keychain_txout(&txn, &changeset.indexer).await?;
        Self::write_locked_outpoints(&txn, &changeset.locked_outpoints).await?;

        txn.commit().await?;
        Ok(())
    }

    /// Insert the wallet network if the database does not already contain one.
    ///
    /// Existing network values are left unchanged. The caller owns `conn` and decides when to
    /// commit or roll back the surrounding transaction.
    pub async fn write_network(
        conn: &DatabaseTransaction<'_>,
        network: Network,
    ) -> Result<(), Error> {
        conn.execute(
            "INSERT OR IGNORE INTO network(id, network) VALUES(0, ?1)",
            [network.to_string()],
        )
        .await?;
        Ok(())
    }

    /// Insert descriptors for the external and internal keychains when they are absent.
    ///
    /// Existing descriptor rows are not replaced. The caller owns `conn` and decides when to
    /// commit or roll back the surrounding transaction.
    pub async fn write_keychain_descriptors(
        conn: &DatabaseTransaction<'_>,
        descriptors: &BTreeMap<KeychainKind, Descriptor<DescriptorPublicKey>>,
    ) -> Result<(), Error> {
        for (keychain, descriptor) in descriptors {
            let keychain = match keychain {
                KeychainKind::External => 0i64,
                KeychainKind::Internal => 1,
            };
            conn.execute(
                "INSERT OR IGNORE INTO keychain(keychain, descriptor) VALUES(?1, ?2)",
                (keychain, descriptor.to_string()),
            )
            .await?;
        }
        Ok(())
    }

    /// Read the complete persisted wallet state as a `ChangeSet`.
    ///
    /// The read is performed in a transaction so its components are read from a consistent
    /// database view. Empty tables produce empty changeset components; the schema must already be
    /// migrated.
    pub async fn read_changeset(&self) -> Result<ChangeSet, Error> {
        let mut conn = self.connect()?;
        let txn = conn.transaction().await?;

        let network = Self::read_network(&txn).await?;
        let descriptors = Self::read_keychain_descriptors(&txn).await?;
        let descriptor = descriptors.get(&KeychainKind::External).cloned();
        let change_descriptor = descriptors.get(&KeychainKind::Internal).cloned();
        let tx_graph = Self::read_tx_graph(&txn).await?;
        let local_chain = Self::read_local_chain(&txn).await?;
        let indexer = Self::read_keychain_txout(&txn).await?;
        let locked_outpoints = Self::read_locked_outpoints(&txn).await?;

        txn.commit().await?;
        Ok(ChangeSet {
            network,
            descriptor,
            change_descriptor,
            tx_graph,
            local_chain,
            indexer,
            locked_outpoints,
        })
    }

    /// Read the stored wallet network, if one has been persisted.
    ///
    /// Returns `Ok(None)` when the network table is empty. The caller owns `conn` and controls the
    /// surrounding transaction.
    pub async fn read_network(conn: &DatabaseTransaction<'_>) -> Result<Option<Network>, Error> {
        let mut rows = conn.query("SELECT network FROM network", ()).await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0)?.parse().map_err(Error::ParseNetwork))
            .transpose()
    }

    /// Read the persisted external and internal descriptors into a keychain-indexed map.
    ///
    /// An empty table produces an empty map. The caller owns `conn` and controls the surrounding
    /// transaction.
    pub async fn read_keychain_descriptors(
        conn: &DatabaseTransaction<'_>,
    ) -> Result<BTreeMap<KeychainKind, Descriptor<DescriptorPublicKey>>, Error> {
        let mut descriptors = BTreeMap::new();
        let mut rows = conn
            .query("SELECT keychain, descriptor FROM keychain", ())
            .await?;
        while let Some(row) = rows.next().await? {
            let keychain = match row.get::<i64>(0)? {
                0 => KeychainKind::External,
                1 => KeychainKind::Internal,
                _ => {
                    debug_assert!(false, "keychain must map to a value of 0 or 1");
                    continue;
                }
            };
            let descriptor = Descriptor::from_str(&row.get::<String>(1)?)?;
            descriptors.insert(keychain, descriptor);
        }
        Ok(descriptors)
    }

    /// Apply locked-outpoint changes to the database.
    ///
    /// Entries marked `true` are inserted; entries marked `false` are removed. The caller owns
    /// `conn` and decides when to commit or roll back the surrounding transaction.
    pub async fn write_locked_outpoints(
        conn: &DatabaseTransaction<'_>,
        locked_outpoints: &locked_outpoints::ChangeSet,
    ) -> Result<(), Error> {
        for (&outpoint, &is_locked) in &locked_outpoints.outpoints {
            let OutPoint { txid, vout } = outpoint;
            if is_locked {
                conn.execute(
                    "INSERT OR IGNORE INTO locked_outpoint(txid, vout) VALUES(?1, ?2)",
                    (txid.to_string(), i64::from(vout)),
                )
                .await?;
            } else {
                conn.execute(
                    "DELETE FROM locked_outpoint WHERE txid = ?1 AND vout = ?2",
                    (txid.to_string(), i64::from(vout)),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Read the outpoints currently recorded as locked.
    ///
    /// The returned changeset contains each stored outpoint with its value set to `true`; unlocked
    /// outpoints are represented by their absence. The caller owns `conn` and controls the
    /// surrounding transaction.
    pub async fn read_locked_outpoints(
        conn: &DatabaseTransaction<'_>,
    ) -> Result<locked_outpoints::ChangeSet, Error> {
        let mut changeset = locked_outpoints::ChangeSet::default();
        let mut rows = conn
            .query("SELECT txid, vout FROM locked_outpoint", ())
            .await?;
        while let Some(row) = rows.next().await? {
            let txid: bitcoin::Txid = row.get::<String>(0)?.parse()?;
            let vout = u32::try_from(row.get::<i64>(1)?)?;
            changeset.outpoints.insert(OutPoint { txid, vout }, true);
        }
        Ok(changeset)
    }
}

type FutureResult<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

impl AsyncWalletPersister for Store {
    type Error = Error;

    fn initialize<'a>(persister: &'a mut Self) -> FutureResult<'a, ChangeSet, Self::Error>
    where
        Self: 'a,
    {
        Box::pin(async {
            persister.migrate().await?;
            persister.read_changeset().await
        })
    }

    fn persist<'a>(
        persister: &'a mut Self,
        changeset: &'a ChangeSet,
    ) -> FutureResult<'a, (), Self::Error>
    where
        Self: 'a,
    {
        Box::pin(async { persister.write_changeset(changeset).await })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn network_table_has_at_most_one_row() -> anyhow::Result<()> {
        let store = Store::new_memory().await?;
        store.migrate().await?;

        let mut conn = store.connect()?;
        let txn = conn.transaction().await?;
        Store::write_network(&txn, Network::Bitcoin).await?;
        Store::write_network(&txn, Network::Bitcoin).await?;
        txn.commit().await?;

        let conn = store.connect()?;
        let mut rows = conn.query("SELECT COUNT(*) FROM network", ()).await?;
        let row = rows.next().await?.expect("expected count row");
        let count: i64 = row.get(0)?;
        assert_eq!(count, 1, "network table should have at most 1 row");
        Ok(())
    }
}
