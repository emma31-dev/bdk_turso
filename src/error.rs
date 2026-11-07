use core::fmt;
use core::num::TryFromIntError;

use bdk_chain::bitcoin;
use bdk_chain::miniscript;
use bitcoin::{consensus, hex::error::HexToArrayError, network::ParseNetworkError};

/// Errors returned while encoding, decoding, parsing, or accessing persisted wallet data.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A stored Bitcoin transaction could not be decoded from consensus encoding.
    Decode(consensus::encode::Error),
    /// A database integer could not be represented by the expected Rust integer type.
    FromInt(TryFromIntError),
    /// A hexadecimal value from persisted data could not be decoded into a fixed-size array.
    HexToArray(HexToArrayError),
    /// A persisted descriptor could not be parsed or validated by Miniscript.
    Miniscript(miniscript::Error),
    /// A persisted PSBT could not be decoded.
    Psbt(bitcoin::psbt::Error),
    /// A PSBT read or write was attempted on a store without local at-rest encryption,
    /// unless the store is a remote sync store whose local cache is not encrypted.
    PsbtRequiresEncryption,
    /// The stored network name could not be parsed as a Bitcoin network.
    ParseNetwork(ParseNetworkError),
    /// A store-to-store transfer could not safely copy the source schema or data.
    Migration(String),
    /// A requested database option is unsupported for the selected store type.
    #[cfg(feature = "remote")]
    UnsupportedOption(String),
    /// A remote push failed after local changes were committed.
    #[cfg(feature = "remote")]
    RemotePush(turso::Error),
    /// A Turso operation failed while opening, querying, or updating the database.
    Turso(turso::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "Decode Error: {e}"),
            Self::FromInt(e) => write!(f, "Integer Conversion Error: {e}"),
            Self::HexToArray(e) => write!(f, "Hex Decode Error: {e}"),
            Self::Miniscript(e) => write!(f, "Miniscript Error: {e}"),
            Self::Psbt(e) => write!(f, "PSBT Error: {e}"),
            Self::PsbtRequiresEncryption => write!(
                f,
                "PSBT persistence requires a database opened with local at-rest encryption"
            ),
            Self::ParseNetwork(e) => write!(f, "Network Parse Error: {e}"),
            #[cfg(feature = "remote")]
            Self::UnsupportedOption(message) => write!(f, "Unsupported Option Error: {message}"),
            Self::Migration(message) => write!(f, "Migration error: {message}"),
            #[cfg(feature = "remote")]
            Self::RemotePush(e) => write!(f, "Remote Push Error: {e}"),
            Self::Turso(e) => write!(f, "Turso Error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

macro_rules! impl_error_from {
    ($from:ty, $to:ident) => {
        impl From<$from> for Error {
            fn from(err: $from) -> Self {
                Self::$to(err)
            }
        }
    };
}

impl_error_from!(consensus::encode::Error, Decode);
impl_error_from!(TryFromIntError, FromInt);
impl_error_from!(HexToArrayError, HexToArray);
impl_error_from!(miniscript::Error, Miniscript);
impl_error_from!(bitcoin::psbt::Error, Psbt);
impl_error_from!(ParseNetworkError, ParseNetwork);
impl_error_from!(turso::Error, Turso);
