//! User-facing copy from the Error mapping table (KTD7, R22). The frontend
//! shows `message` verbatim and never formats a reason itself.

use crate::types::ReasonCode;

pub const DEVICE_GONE: &str = "The radio was unplugged during the boot. Plug it back in. PocketBoot will check its startup data before trying again.";
pub const BUSY: &str = "Another app is using the radio. Quit Betaflight Configurator, STM32CubeProgrammer, or similar apps, then click Try again.";
pub const NOT_WRITEABLE: &str = "The radio refused the write. This usually means the device isn't a RadioMaster Pocket. Nothing was changed.";
pub const USB_IO: &str =
    "The USB connection dropped out. Try a different cable or port, then click Try again.";
pub const INTERNAL: &str = "Something went wrong inside PocketBoot. Click Try again. If it keeps happening, use Copy diagnostics and report it.";
pub const TIMEOUT: &str =
    "The radio stopped responding. Unplug it, plug it back in, and PocketBoot will start over.";
pub const VALIDATION_SIZE: &str =
    "The safety copy came back incomplete. Nothing was changed on the radio.";
pub const VALIDATION_BLANK: &str = "The safety copy is blank, so PocketBoot won't write it back. Nothing was changed on the radio. If this keeps happening, the radio may need EdgeTX reinstalled.";
pub const HASH_MISMATCH: &str =
    "The safety copy changed on disk. PocketBoot will make a fresh one.";
pub const UNKNOWN: &str = "Something unexpected happened. Click Try again. If it keeps happening, use Copy diagnostics and report it.";
pub const FATAL: &str = "A required part of the app is missing or blocked. Download PocketBoot again and replace this copy.";

pub fn for_reason(r: ReasonCode) -> &'static str {
    use ReasonCode::*;
    match r {
        DeviceGone => DEVICE_GONE,
        // The table gives `LIBUSB_ERROR_ACCESS` the same copy as busy.
        AccessDenied | Busy => BUSY,
        NotWriteable => NOT_WRITEABLE,
        UsbIo => USB_IO,
        Timeout => TIMEOUT,
        ValidationSize => VALIDATION_SIZE,
        ValidationBlank => VALIDATION_BLANK,
        HashMismatch => HASH_MISMATCH,
        SidecarMissing => FATAL,
        Unknown => UNKNOWN,
    }
}

/// `canRetry: false` rows of the table: retrying cannot help without a replug.
pub fn can_retry(r: ReasonCode) -> bool {
    !matches!(r, ReasonCode::Timeout | ReasonCode::NotWriteable)
}
