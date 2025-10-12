/// A custom error type for the bootloader (UEFI/BIOS).
#[derive(Debug, Clone, Copy)]
pub enum BellowsError {
    Efi { status: super::uefi::EfiStatus },
    FileIo(&'static str),
    PeParse(&'static str),
    AllocationFailed(&'static str),
    InvalidState(&'static str),
    ProtocolNotFound(&'static str),
}

impl From<super::uefi::EfiStatus> for BellowsError {
    fn from(status: super::uefi::EfiStatus) -> Self {
        Self::Efi { status }
    }
}

pub type Result<T> = core::result::Result<T, BellowsError>;
