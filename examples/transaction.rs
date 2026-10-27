use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use bdk_bitcoind_rpc::{
    Emitter,
    bitcoincore_rpc::{Auth, Client, RpcApi},
};
use bdk_turso::Store;
use bdk_wallet::{
    KeychainKind, SignOptions, Wallet,
    bitcoin::{Address, Amount, FeeRate, Network},
};

const DB_PATH: &str = "polar.db";
const NETWORK: Network = Network::Regtest;
const RPC_URL: &str = "http://127.0.0.1:18443";
const EXTERNAL_DESC: &str = "wpkh([dae5422f/84'/1'/0'/0]tprv8iPUhjo2Rrs6maF8A3EuTbp8JxxtsuHU4Fwfdk6Qz74G4HqBwy38rpfwKpGyunHdTqxig4kMScbiTFf5h1NmekZfiVNeHEPrwESWMCLU2B8/*)";
const INTERNAL_DESC: &str = "wpkh([dae5422f/84'/1'/0'/1]tprv8iPUhjo2Rrs6oiZHqEo3GdtJhHXy3uyFGPg7VaFehs4PhAGiivUG7XHWsdK6W7G1VygXkuJ1Cau5mV6uiy7EfviZkHcAg7a4yyYoJpV59Ub/*)";

// This uses BDK 3.0's deprecated wallet-owned signer for a regtest-only demonstration.
#[allow(deprecated)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rpc_url = std::env::var("POLAR_RPC_URL").unwrap_or_else(|_| RPC_URL.to_owned());
    let rpc_user = std::env::var("POLAR_RPC_USER")?;
    let rpc_pass = std::env::var("POLAR_RPC_PASS")?;
    let rpc = Client::new(&rpc_url, Auth::UserPass(rpc_user, rpc_pass))?;

    let mut store = Store::new(DB_PATH).await?;
    let mut wallet = match Wallet::load()
        .descriptor(KeychainKind::External, Some(EXTERNAL_DESC))
        .descriptor(KeychainKind::Internal, Some(INTERNAL_DESC))
        .extract_keys()
        .check_network(NETWORK)
        .load_wallet_async(&mut store)
        .await?
    {
        Some(wallet) => wallet,
        None => {
            Wallet::create(EXTERNAL_DESC, INTERNAL_DESC)
                .network(NETWORK)
                .create_wallet_async(&mut store)
                .await?
        }
    };

    let wallet_tip = wallet.latest_checkpoint();
    let expected_mempool_txs = wallet
        .transactions()
        .map(|tx| tx.tx_node.tx.clone())
        .collect::<Vec<_>>();
    let mut emitter = Emitter::new(
        &rpc,
        wallet_tip.clone(),
        wallet_tip.height(),
        expected_mempool_txs,
    );
    while let Some(block) = emitter.next_block()? {
        wallet.apply_block_connected_to(
            &block.block,
            block.block_height(),
            block.connected_to(),
        )?;
    }
    wallet.persist_async(&mut store).await?;

    if wallet.list_unspent().next().is_none() {
        let receive_address = wallet.next_unused_address(KeychainKind::External);
        wallet.persist_async(&mut store).await?;
        println!(
            "No spendable funds. Fund this regtest address, mine 101 blocks, then rerun: {receive_address}"
        );
        return Ok(());
    }

    let recipient =
        Address::from_str(&std::env::var("RECIPIENT_ADDRESS")?)?.require_network(NETWORK)?;
    let mut builder = wallet.build_tx();
    builder
        .add_recipient(recipient.script_pubkey(), Amount::from_sat(10_000))
        .fee_rate(FeeRate::from_sat_per_vb(2).expect("valid fee rate"));
    let psbt = builder.finish()?;

    // Persist the newly revealed change index before the transaction can be broadcast.
    wallet.persist_async(&mut store).await?;

    let psbt_txid = store.write_psbt(&psbt).await?;
    let mut restored_psbt = store
        .read_psbt(psbt_txid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("stored PSBT was not found"))?;
    anyhow::ensure!(
        restored_psbt == psbt,
        "PSBT changed during database round-trip"
    );

    anyhow::ensure!(
        wallet.sign(&mut restored_psbt, SignOptions::default())?,
        "wallet could not fully sign the transaction"
    );
    store.write_psbt(&restored_psbt).await?;

    let tx = restored_psbt.extract_tx()?;
    let txid = rpc.send_raw_transaction(&tx)?;
    let last_seen = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    wallet.apply_unconfirmed_txs([(tx, last_seen)]);
    wallet.persist_async(&mut store).await?;

    println!("Broadcast transaction: {txid}");
    Ok(())
}
