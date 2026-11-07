//! Shared configuration for the examples.
//!
//! Both examples read `examples/config.toml`. Copy `examples/example.toml` to that path and edit
//! the copy; the local file is ignored by git.

use std::{
    fmt, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, anyhow};
use bdk_wallet::{
    bitcoin::{Address, Network, secp256k1::Secp256k1},
    descriptor::IntoWalletDescriptor,
};

/// Settings shared by the wallet and transaction examples.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub database_path: String,
    pub network: String,
    pub rpc: RpcConfig,
    pub descriptors: DescriptorConfig,
    pub recipient_address: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcConfig {
    pub url: String,
    pub user: String,
    pub password: String,
}

impl fmt::Debug for RpcConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcConfig")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescriptorConfig {
    pub external: String,
    pub internal: String,
}

impl Config {
    /// Load `examples/config.toml` from the crate root.
    #[allow(dead_code)] // Not exercised by the config tests, which load explicit paths.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::default_path())
    }

    /// Path of the local configuration file expected by the examples.
    #[allow(dead_code)]
    pub fn default_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/config.toml")
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(anyhow!(
                    "could not read {}; copy examples/example.toml to examples/config.toml and edit it",
                    path.display()
                ));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()));
            }
        };
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML config at {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let network = self.network()?;
        let secp = Secp256k1::new();
        for (field, descriptor) in [
            ("descriptors.external", self.descriptors.external.as_str()),
            ("descriptors.internal", self.descriptors.internal.as_str()),
        ] {
            descriptor
                .into_wallet_descriptor(&secp, network.into())
                .with_context(|| format!("invalid `{field}` in config"))?;
        }
        if let Some(value) = self.recipient_address.as_deref() {
            Address::from_str(value)
                .with_context(|| format!("invalid `recipient_address` in config: {value}"))?
                .require_network(network)
                .with_context(|| {
                    format!("`recipient_address` is not valid on {network}: {value}")
                })?;
        }
        Ok(())
    }

    pub fn network(&self) -> Result<Network> {
        self.network.parse().with_context(|| {
            format!(
                "invalid network `{}` in config; expected bitcoin, testnet, testnet4, signet, or regtest",
                self.network
            )
        })
    }

    #[allow(dead_code)] // Used by the transaction example only.
    pub fn recipient_address(&self, network: Network) -> Result<Address> {
        let value = self.recipient_address.as_deref().context(
            "recipient_address is missing from examples/config.toml; add one to send a payment",
        )?;
        Address::from_str(value)
            .with_context(|| format!("invalid recipient_address `{value}` in config"))?
            .require_network(network)
            .with_context(|| format!("recipient_address `{value}` is not valid on {network}"))
    }
}
