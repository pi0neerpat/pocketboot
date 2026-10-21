//! Core types shared by `dfu`, `backup`, and `machine` (KTD11).

use serde::{Deserialize, Serialize};

/// Why an operation failed. Serialized as the `ReasonCode` union in the
/// frontend schema. User-facing copy for each code lives in `messages.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasonCode {
    DeviceGone,
    AccessDenied,
    Busy,
    NotWriteable,
    UsbIo,
    Timeout,
    ValidationSize,
    ValidationBlank,
    HashMismatch,
    SidecarMissing,
    Unknown,
}

/// A USB serial as reported by the bootloader. `None` at the call sites means
/// the device exposes no serial (Q1 fallback, R13).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Serial(String);

impl Serial {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Serial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
