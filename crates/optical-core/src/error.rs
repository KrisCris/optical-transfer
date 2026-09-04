use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpticalError {
    InvalidFrame,
    UnsupportedDimensions,
    InvalidContainer,
    SizeLimit,
    ChecksumFailure,
    CompressionFailure,
    IncompleteTransfer,
}

impl fmt::Display for OpticalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFrame => "invalid optical frame",
            Self::UnsupportedDimensions => "unsupported optical dimensions",
            Self::InvalidContainer => "invalid optical container",
            Self::SizeLimit => "optical payload exceeds its size limit",
            Self::ChecksumFailure => "optical checksum verification failed",
            Self::CompressionFailure => "optical compression operation failed",
            Self::IncompleteTransfer => "optical transfer is incomplete",
        })
    }
}

impl std::error::Error for OpticalError {}
