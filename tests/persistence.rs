use std::sync::Arc;

use bdk_chain::{
    BlockId, ConfirmationBlockTime, DescriptorExt, Merge,
    bitcoin::{
        Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxIn, TxOut, Txid, absolute,
        hashes::Hash, transaction,
    },
    keychain_txout, local_chain,
    miniscript::descriptor::{Descriptor, DescriptorPublicKey},
    tx_graph,
};
use bdk_turso::{EncryptionOpts, Options, Store};
use bdk_wallet::{ChangeSet, KeychainKind, Wallet, locked_outpoints};

const EXTERNAL_DESCRIPTOR: &str = "wpkh([41f2aed0/84h/1h/0h]tpubDDFSdQWw75hk1ewbwnNpPp5DvXFRKt68ioPoyJDY752cNHKkFxPWqkqCyCf4hxrEfpuxh46QisehL3m8Bi6MsAv394QVLopwbtfvryFQNUH/0/*)#g0w0ymmw";
const INTERNAL_DESCRIPTOR: &str = "wpkh([41f2aed0/84h/1h/0h]tpubDDFSdQWw75hk1ewbwnNpPp5DvXFRKt68ioPoyJDY752cNHKkFxPWqkqCyCf4hxrEfpuxh46QisehL3m8Bi6MsAv394QVLopwbtfvryFQNUH/1/*)#emtwewtk";

#[tokio::test]
async fn experimental_options_are_disabled_by_default() -> anyhow::Result<()> {
    let store = Store::new_memory().await?;
    let options = store.options();

    assert!(!options.index_method);
    assert!(!options.multiprocess_wal);
    assert!(!options.vacuum);
    assert!(!options.attach);
    assert!(!options.generated_columns);
    assert!(!options.without_rowid);
    assert!(!options.mvcc_passive_checkpoint);
    assert!(options.encryption.is_none());
    Ok(())
}

#[tokio::test]
async fn store_opens_with_experimental_options() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("options.db");
    let path = path.to_str().expect("temporary path is valid UTF-8");
    let options = Options {
        index_method: true,
        multiprocess_wal: true,
        vacuum: true,
        attach: true,
        generated_columns: true,
        without_rowid: true,
        mvcc_passive_checkpoint: true,
        encryption: None,
    };

    let store = Store::new_with_options(path, options).await?;
    assert!(store.options().index_method);
    assert!(store.options().multiprocess_wal);
    assert!(store.options().vacuum);
    assert!(store.options().attach);
    assert!(store.options().generated_columns);
    assert!(store.options().without_rowid);
    assert!(store.options().mvcc_passive_checkpoint);
    store.migrate().await?;
    Ok(())
}

#[tokio::test]
async fn store_opens_with_encryption_options() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("encrypted.db");
    let path = path.to_str().expect("temporary path is valid UTF-8");
    let options = Options {
        encryption: Some(EncryptionOpts {
            cipher: "aes256gcm".to_owned(),
            hexkey: "00".repeat(32),
        }),
        ..Options::default()
    };

    let store = Store::new_with_options(path, options).await?;
    assert!(store.options().encryption.is_some());
    store.migrate().await?;
    Ok(())
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

fn transaction(seed: u8) -> Arc<Transaction> {
    Arc::new(Transaction {
        version: transaction::Version::ONE,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([seed; 32]), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50_000 + u64::from(seed)),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51, seed]),
        }],
    })
}

fn graph_changeset(seed: u8) -> tx_graph::ChangeSet<ConfirmationBlockTime> {
    let tx = transaction(seed);
    let txid = tx.compute_txid();
    let hash = BlockHash::from_byte_array([seed.wrapping_add(1); 32]);
    let anchor = ConfirmationBlockTime {
        block_id: BlockId {
            height: 100 + u32::from(seed),
            hash,
        },
        confirmation_time: 1_700_000_000 + u64::from(seed),
    };

    tx_graph::ChangeSet {
        txs: [tx.clone()].into(),
        txouts: [(OutPoint::new(txid, 0), tx.output[0].clone())].into(),
        anchors: [(anchor, txid)].into(),
        first_seen: [(txid, 1_000 + u64::from(seed))].into(),
        last_seen: [(txid, 2_000 + u64::from(seed))].into(),
        last_evicted: [(txid, 3_000 + u64::from(seed))].into(),
    }
}

fn full_changeset(seed: u8) -> ChangeSet {
    let (external, internal) = descriptors();
    let external_id = external.descriptor_id();
    let internal_id = internal.descriptor_id();
    let tx = transaction(seed);
    let txid = tx.compute_txid();
    let block_hash = BlockHash::from_byte_array([seed.wrapping_add(1); 32]);
    let height = 100 + u32::from(seed);

    ChangeSet {
        descriptor: Some(external),
        change_descriptor: Some(internal),
        network: Some(Network::Signet),
        local_chain: local_chain::ChangeSet {
            blocks: [(height, Some(block_hash))].into(),
        },
        tx_graph: graph_changeset(seed),
        indexer: keychain_txout::ChangeSet {
            last_revealed: [(external_id, 5 + u32::from(seed)), (internal_id, 3)].into(),
            spk_cache: [
                (
                    external_id,
                    [(0, ScriptBuf::from_bytes(vec![0x51, seed]))].into(),
                ),
                (
                    internal_id,
                    [(1, ScriptBuf::from_bytes(vec![0x52, seed]))].into(),
                ),
            ]
            .into(),
        },
        locked_outpoints: locked_outpoints::ChangeSet {
            outpoints: [(OutPoint::new(txid, 0), true)].into(),
        },
    }
}

#[tokio::test]
async fn full_changeset_round_trips() -> anyhow::Result<()> {
    let store = Store::new_memory().await?;
    store.migrate().await?;
    store.migrate().await?;

    let expected = full_changeset(1);
    store.write_changeset(&expected).await?;

    assert_eq!(store.read_changeset().await?, expected);
    Ok(())
}

#[tokio::test]
async fn incremental_changesets_merge_without_losing_entries() -> anyhow::Result<()> {
    let store = Store::new_memory().await?;
    store.migrate().await?;

    let initial = full_changeset(2);
    let (external, _) = descriptors();
    let external_id = external.descriptor_id();
    let new_tx = transaction(3);
    let new_txid = new_tx.compute_txid();
    let mut next = ChangeSet {
        network: Some(Network::Signet),
        tx_graph: graph_changeset(3),
        ..ChangeSet::default()
    };
    next.local_chain
        .blocks
        .insert(200, Some(BlockHash::from_byte_array([200; 32])));
    next.indexer.last_revealed.insert(external_id, 99);
    next.indexer
        .spk_cache
        .entry(external_id)
        .or_default()
        .insert(99, ScriptBuf::from_bytes(vec![0x53, 0x54]));
    next.locked_outpoints
        .outpoints
        .insert(OutPoint::new(new_txid, 0), true);

    let mut expected = initial.clone();
    expected.merge(next.clone());
    store.write_changeset(&initial).await?;
    store.write_changeset(&next).await?;

    assert_eq!(store.read_changeset().await?, expected);
    Ok(())
}

#[tokio::test]
async fn wallet_async_persister_flow_reloads_revealed_address() -> anyhow::Result<()> {
    let mut store = Store::new_memory().await?;
    let mut wallet = Wallet::create(EXTERNAL_DESCRIPTOR, INTERNAL_DESCRIPTOR)
        .network(Network::Signet)
        .create_wallet_async(&mut store)
        .await?;

    let expected_address = wallet
        .reveal_next_address(KeychainKind::External)
        .to_string();
    wallet.persist_async(&mut store).await?;

    let mut loaded = Wallet::load()
        .descriptor(KeychainKind::External, Some(EXTERNAL_DESCRIPTOR))
        .descriptor(KeychainKind::Internal, Some(INTERNAL_DESCRIPTOR))
        .check_network(Network::Signet)
        .load_wallet_async(&mut store)
        .await?
        .expect("wallet should reload from Turso");

    assert_eq!(
        loaded
            .next_unused_address(KeychainKind::External)
            .to_string(),
        expected_address
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_writes_preserve_both_changesets() -> anyhow::Result<()> {
    let store = Store::new_memory().await?;
    store.migrate().await?;

    let mut first = ChangeSet::default();
    first.tx_graph = graph_changeset(11);
    let mut second = ChangeSet::default();
    second.tx_graph = graph_changeset(12);

    let (first_result, second_result) = tokio::join!(
        store.write_changeset(&first),
        store.write_changeset(&second),
    );
    first_result?;
    second_result?;

    let mut expected = first;
    expected.merge(second);
    assert_eq!(store.read_changeset().await?, expected);
    Ok(())
}

#[tokio::test]
async fn file_reopens_and_is_readable_by_bdk_sqlite() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("wallet.db");
    let path = path.to_str().expect("temporary path is valid UTF-8");
    let expected = full_changeset(20);

    let store = Store::new(path).await?;
    store.migrate().await?;
    store.write_changeset(&expected).await?;
    drop(store);

    let store = Store::new(path).await?;
    store.migrate().await?;
    assert_eq!(store.read_changeset().await?, expected);
    drop(store);

    let sqlite_store = bdk_sqlite::Store::new(path).await?;
    assert_eq!(sqlite_store.read_changeset().await?, expected);
    Ok(())
}
