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

/// One USB device exposing a DFU interface (class 0xFE, subclass 0x01), any
/// VID/PID (R17). Produced by the watcher from read-only enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfuDeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<Serial>,
    pub product: Option<String>,
    pub location_id: u32,
}

impl DfuDeviceInfo {
    /// Only `0483:df11` is ever acted on (R16).
    pub fn is_pocket(&self) -> bool {
        self.vid == 0x0483 && self.pid == 0xdf11
    }

    /// Whether `other` is the same physical device: by serial when the
    /// bootloader exposes one, else by port location (KTD4).
    pub fn same_device(&self, other: &Self) -> bool {
        (self.vid, self.pid) == (other.vid, other.pid)
            && match (&self.serial, &other.serial) {
                (Some(a), Some(b)) => a == b,
                (None, None) => self.location_id == other.location_id,
                _ => false,
            }
    }
}

/// Every DFU-class device present at one poll.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub dfu_devices: Vec<DfuDeviceInfo>,
}
