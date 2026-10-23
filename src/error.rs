use core::fmt;
use core::num::TryFromIntError;

use bdk_chain::bitcoin;
use bdk_chain::miniscript;
use bitcoin::{consensus, hex::error::HexToArrayError, network::ParseNetworkError};

/// Crate error.
#[derive(Debug)]
pub enum Error {
    /// `bitcoin` consensus encoding error.
    Decode(consensus::encode::Error),
    /// error converting an integer.
    FromInt(TryFromIntError),
    /// `bitcoin` hex to array error.
    HexToArray(HexToArrayError),
    /// `miniscript` error.
    Miniscript(miniscript::Error),
    /// parse `Network` error.
    ParseNetwork(ParseNetworkError),
    /// Turso database error.
    Turso(turso::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "{e}"),
            Self::FromInt(e) => write!(f, "{e}"),
            Self::HexToArray(e) => write!(f, "{e}"),
            Self::Miniscript(e) => write!(f, "{e}"),
            Self::ParseNetwork(e) => write!(f, "{e}"),
            Self::Turso(e) => write!(f, "{e}"),
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
impl_error_from!(ParseNetworkError, ParseNetwork);
impl_error_from!(turso::Error, Turso);
