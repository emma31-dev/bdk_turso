#[path = "../examples/common/mod.rs"]
mod common;

use std::{
    fs,
    path::{Path, PathBuf},
};

use bdk_wallet::bitcoin::Network;
use common::Config;

fn template_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/example.toml")
}

fn config_with(replacement: (&str, &str)) -> anyhow::Result<tempfile::TempDir> {
    let contents = fs::read_to_string(template_path())?;
    let dir = tempfile::tempdir()?;
    fs::write(
        dir.path().join("config.toml"),
        contents.replace(replacement.0, replacement.1),
    )?;
    Ok(dir)
}

#[test]
fn template_config_is_valid() -> anyhow::Result<()> {
    let config = Config::load_from(&template_path())?;

    assert_eq!(config.database_path, "polar.db");
    assert_eq!(config.network()?, Network::Regtest);
    assert_eq!(config.rpc.url, "http://127.0.0.1:18443");
    assert_eq!(config.rpc.user, "polaruser");
    assert_eq!(config.rpc.password, "polarpassword");
    assert!(config.recipient_address.is_none());
    Ok(())
}

#[test]
fn missing_config_explains_how_to_create_it() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let error = Config::load_from(&dir.path().join("config.toml"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("copy examples/example.toml"), "{error}");
    Ok(())
}

#[test]
fn unknown_field_is_rejected() -> anyhow::Result<()> {
    let dir = config_with((
        "network = \"regtest\"",
        "network = \"regtest\"\ntypo = \"x\"",
    ))?;

    let error = Config::load_from(&dir.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid TOML config"), "{error}");
    Ok(())
}

#[test]
fn invalid_recipient_is_rejected() -> anyhow::Result<()> {
    let dir = config_with((
        "# recipient_address = \"bcrt1q...\"",
        "recipient_address = \"not-an-address\"",
    ))?;

    let error = Config::load_from(&dir.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid `recipient_address`"), "{error}");
    Ok(())
}

#[test]
fn valid_recipient_is_accepted() -> anyhow::Result<()> {
    let dir = config_with((
        "# recipient_address = \"bcrt1q...\"",
        "recipient_address = \"bcrt1q3qtze4ys45tgdvguj66zrk4fu6hq3a3v9pfly5\"",
    ))?;
    let config = Config::load_from(&dir.path().join("config.toml"))?;

    assert_eq!(
        config.recipient_address(Network::Regtest)?.to_string(),
        "bcrt1q3qtze4ys45tgdvguj66zrk4fu6hq3a3v9pfly5"
    );
    Ok(())
}

#[test]
fn debug_redacts_rpc_password() -> anyhow::Result<()> {
    let config = Config::load_from(&template_path())?;
    let debug = format!("{config:?}");

    assert!(!debug.contains("polarpassword"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
    Ok(())
}

#[test]
fn encryption_key_is_optional_for_wallet_example() -> anyhow::Result<()> {
    let config = Config::load_from(&template_path())?;

    assert!(config.store_options().is_ok());
    let error = config.encrypted_store_options().unwrap_err().to_string();
    assert!(error.contains("encryption_hexkey is required"), "{error}");
    Ok(())
}

#[test]
fn invalid_encryption_key_is_rejected() -> anyhow::Result<()> {
    let dir = config_with((
        "# encryption_hexkey = \"...\"",
        "encryption_hexkey = \"zz\"",
    ))?;

    let error = Config::load_from(&dir.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid `encryption_hexkey`"), "{error}");
    Ok(())
}

#[test]
fn valid_encryption_key_is_accepted() -> anyhow::Result<()> {
    let key = "0".repeat(64);
    let replacement = format!("encryption_hexkey = \"{key}\"");
    let dir = config_with(("# encryption_hexkey = \"...\"", &replacement))?;
    let config = Config::load_from(&dir.path().join("config.toml"))?;

    assert!(config.encrypted_store_options().is_ok());
    Ok(())
}

#[test]
fn malformed_toml_is_rejected() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config.toml");
    fs::write(&path, "not = [valid")?;

    let error = Config::load_from(&path).unwrap_err().to_string();
    assert!(error.contains("invalid TOML config"), "{error}");
    Ok(())
}

#[test]
fn missing_required_field_is_rejected() -> anyhow::Result<()> {
    let dir = config_with(("user = \"polaruser\"\n", ""))?;

    let error = Config::load_from(&dir.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid TOML config"), "{error}");
    Ok(())
}

#[test]
fn unsupported_network_is_rejected() -> anyhow::Result<()> {
    let dir = config_with(("network = \"regtest\"", "network = \"main\""))?;

    let error = Config::load_from(&dir.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid network `main`"), "{error}");
    Ok(())
}

#[test]
fn recipient_is_optional_until_requested() -> anyhow::Result<()> {
    let config = Config::load_from(&template_path())?;

    let error = config
        .recipient_address(Network::Regtest)
        .unwrap_err()
        .to_string();
    assert!(error.contains("recipient_address is missing"), "{error}");
    Ok(())
}
