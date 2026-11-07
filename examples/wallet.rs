mod common;

use bdk_bitcoind_rpc::{
    Emitter, NO_EXPECTED_MEMPOOL_TXS,
    bitcoincore_rpc::{Auth, Client},
};
use bdk_turso::Store;
use bdk_wallet::{KeychainKind, Wallet};
use common::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load()?;
    let network = config.network()?;
    let rpc = Client::new(
        &config.rpc.url,
        Auth::UserPass(config.rpc.user.clone(), config.rpc.password.clone()),
    )?;

    let mut store = Store::new_with_options(&config.database_path, config.store_options()?).await?;
    let mut wallet = match Wallet::load()
        .descriptor(
            KeychainKind::External,
            Some(config.descriptors.external.clone()),
        )
        .descriptor(
            KeychainKind::Internal,
            Some(config.descriptors.internal.clone()),
        )
        .check_network(network)
        .load_wallet_async(&mut store)
        .await?
    {
        Some(wallet) => wallet,
        None => {
            Wallet::create(
                config.descriptors.external.clone(),
                config.descriptors.internal.clone(),
            )
            .network(network)
            .create_wallet_async(&mut store)
            .await?
        }
    };

    let address = wallet.reveal_next_address(KeychainKind::External);
    wallet.persist_async(&mut store).await?;
    println!("Receive address: {}", address.address);

    let wallet_tip = wallet.latest_checkpoint();
    let mut emitter = Emitter::new(
        &rpc,
        wallet_tip.clone(),
        wallet_tip.height(),
        NO_EXPECTED_MEMPOOL_TXS,
    );
    let mut blocks_synced = 0;
    while let Some(block) = emitter.next_block()? {
        wallet.apply_block_connected_to(
            &block.block,
            block.block_height(),
            block.connected_to(),
        )?;
        blocks_synced += 1;
    }

    wallet.persist_async(&mut store).await?;
    println!("Synced {blocks_synced} blocks from Bitcoin Core RPC");
    println!("Balance: {}", wallet.balance().total());
    Ok(())
}
