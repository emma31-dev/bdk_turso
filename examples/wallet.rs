use bdk_turso::Store;
use bdk_wallet::bitcoin;
use bdk_wallet::{KeychainKind, Wallet};

const DB_PATH: &str = "test.db";
const NETWORK: bitcoin::Network = bitcoin::Network::Testnet4;
const EXTERNAL_DESC: &str = "wpkh([e273fe42/84'/1'/0']tpubDCmr3Luq75npLaYmRqqW1rLfSbfpnBXwLwAmUbR333fp95wjCHar3zoc9zSWovZFwrWr53mm3NTVqt6d1Pt6G26uf4etQjc3Pr5Hxe9QEQ2/0/*)";
const INTERNAL_DESC: &str = "wpkh([e273fe42/84'/1'/0']tpubDCmr3Luq75npLaYmRqqW1rLfSbfpnBXwLwAmUbR333fp95wjCHar3zoc9zSWovZFwrWr53mm3NTVqt6d1Pt6G26uf4etQjc3Pr5Hxe9QEQ2/1/*)";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut store = Store::new(DB_PATH).await?;
    let mut wallet = match Wallet::load().load_wallet_async(&mut store).await? {
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

    println!("Revealed address: {address}");
    println!("Balance: {}", wallet.balance().total());
    Ok(())
}
