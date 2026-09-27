//! First-party decompression shared by source containers and durable recovery.
//! Callers retain admission, provenance and publication ownership; codecs reject
//! malformed framing and never convert cancellation into successful completion.

use rootlight_cancel::Cancelled;
use std::fmt;

mod deflate;
pub(crate) mod gzip;
pub(crate) use deflate::inflate;

#[derive(Debug)]
pub(crate) enum Error {
    Deflate { input_offset: usize },
    OutputSize,
    Allocation,
    Cancelled(Cancelled),
}

impl From<Cancelled> for Error {
    fn from(error: Cancelled) -> Self {
        Self::Cancelled(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deflate { input_offset } => write!(
                formatter,
                "invalid deflate stream at encoded byte {input_offset}"
            ),
            Self::OutputSize => formatter.write_str("expanded size differs from its declaration"),
            Self::Allocation => formatter.write_str("bounded decompression allocation failed"),
            Self::Cancelled(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cancelled(error) => Some(error),
            _ => None,
        }
    }
}
