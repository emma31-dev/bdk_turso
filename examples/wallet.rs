#![allow(unused)]

use std::collections::BTreeSet;
use std::io::Write;

use bdk_esplora::EsploraAsyncExt;
use bdk_esplora::esplora_client;
use bdk_turso::Store;
use bdk_wallet::bitcoin;
use bdk_wallet::{KeychainKind, Wallet};

const DB_PATH: &str = "test.db";
const NETWORK: bitcoin::Network = bitcoin::Network::Testnet4;
const EXTERNAL_DESC: &str = "wpkh([e273fe42/84'/1'/0']tpubDCmr3Luq75npLaYmRqqW1rLfSbfpnBXwLwAmUbR333fp95wjCHar3zoc9zSWovZFwrWr53mm3NTVqt6d1Pt6G26uf4etQjc3Pr5Hxe9QEQ2/0/*)";
const INTERNAL_DESC: &str = "wpkh([e273fe42/84'/1'/0']tpubDCmr3Luq75npLaYmRqqW1rLfSbfpnBXwLwAmUbR333fp95wjCHar3zoc9zSWovZFwrWr53mm3NTVqt6d1Pt6G26uf4etQjc3Pr5Hxe9QEQ2/1/*)";
const ESPLORA_URL: &str = "https://mempool.space/testnet4/api";
const STOP_GAP: usize = 20;
const PARALLEL_REQUESTS: usize = 1;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut db = Store::new(DB_PATH).await?;

    let mut wallet = match Wallet::load().load_wallet_async(&mut db).await? {
        Some(wallet) => wallet,
        None => {
            Wallet::create(EXTERNAL_DESC, INTERNAL_DESC)
                .network(NETWORK)
                .create_wallet_async(&mut db)
                .await?
        }
    };

    let client = esplora_client::Builder::new(ESPLORA_URL).build_async()?;
    let request = wallet.start_full_scan().inspect({
        let mut once = BTreeSet::<KeychainKind>::new();
        move |keychain, spk_i, _| {
            if once.insert(keychain) {
                print!("\nScanning keychain [{keychain:?}]");
            }
            print!(" {spk_i:<3}");
            std::io::stdout().flush().unwrap();
        }
    });
    let update = client
        .full_scan(request, STOP_GAP, PARALLEL_REQUESTS)
        .await?;

    wallet.apply_update(update)?;
    println!();
    println!(
        "Address: {}",
        wallet.next_unused_address(KeychainKind::External)
    );
    wallet.persist_async(&mut db).await?;
    println!("{}", wallet.balance().total());

    Ok(())
}
