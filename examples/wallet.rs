use bdk_bitcoind_rpc::{
    Emitter, NO_EXPECTED_MEMPOOL_TXS,
    bitcoincore_rpc::{Auth, Client},
};
use bdk_turso::Store;
use bdk_wallet::bitcoin;
use bdk_wallet::{KeychainKind, Wallet};

const DB_PATH: &str = "polar.db";
const NETWORK: bitcoin::Network = bitcoin::Network::Regtest;
const POLAR_RPC_URL: &str = "http://127.0.0.1:18443";
const EXTERNAL_DESC: &str = "wpkh([dae5422f/84'/1'/0'/0]tprv8iPUhjo2Rrs6maF8A3EuTbp8JxxtsuHU4Fwfdk6Qz74G4HqBwy38rpfwKpGyunHdTqxig4kMScbiTFf5h1NmekZfiVNeHEPrwESWMCLU2B8/*)";
const INTERNAL_DESC: &str = "wpkh([dae5422f/84'/1'/0'/1]tprv8iPUhjo2Rrs6oiZHqEo3GdtJhHXy3uyFGPg7VaFehs4PhAGiivUG7XHWsdK6W7G1VygXkuJ1Cau5mV6uiy7EfviZkHcAg7a4yyYoJpV59Ub/*)";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rpc_url = std::env::var("POLAR_RPC_URL").unwrap_or_else(|_| POLAR_RPC_URL.to_owned());
    let rpc_user = std::env::var("POLAR_RPC_USER")?;
    let rpc_pass = std::env::var("POLAR_RPC_PASS")?;
    let rpc = Client::new(&rpc_url, Auth::UserPass(rpc_user, rpc_pass))?;

    let mut store = Store::new(DB_PATH).await?;
    let mut wallet = match Wallet::load()
        .descriptor(KeychainKind::External, Some(EXTERNAL_DESC))
        .descriptor(KeychainKind::Internal, Some(INTERNAL_DESC))
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
