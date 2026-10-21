//! Read-only USB enumeration with `nusb` (KTD4, R24). Never opens or claims
//! a device.

use std::time::Duration;

use nusb::MaybeFuture;
use tokio::sync::mpsc::Sender;

use crate::types::{DeviceSnapshot, DfuDeviceInfo, Serial};

pub const POLL: Duration = Duration::from_millis(500);

/// The enumeration facts a snapshot needs, decoupled from `nusb::DeviceInfo`
/// (which has no public constructor) so the filter is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub product: Option<String>,
    pub location_id: u32,
    /// `(class, subclass)` per interface.
    pub interfaces: Vec<(u8, u8)>,
}

impl From<&nusb::DeviceInfo> for UsbDevice {
    fn from(d: &nusb::DeviceInfo) -> Self {
        Self {
            vid: d.vendor_id(),
            pid: d.product_id(),
            serial: d.serial_number().map(str::to_string),
            product: d.product_string().map(str::to_string),
            location_id: d.location_id(),
            interfaces: d.interfaces().map(|i| (i.class(), i.subclass())).collect(),
        }
    }
}

const DFU_CLASS: (u8, u8) = (0xfe, 0x01);

/// Any device exposing a DFU interface, whatever its VID (R24: the
/// non-Pocket conflict check needs to see them). Sorted so that an
/// unchanged bus yields an equal snapshot for the machine's debounce.
pub fn snapshot_from(devices: impl IntoIterator<Item = UsbDevice>) -> DeviceSnapshot {
    let mut dfu_devices: Vec<DfuDeviceInfo> = devices
        .into_iter()
        .filter(|d| d.interfaces.contains(&DFU_CLASS))
        .map(|d| DfuDeviceInfo {
            vid: d.vid,
            pid: d.pid,
            serial: d.serial.filter(|s| !s.trim().is_empty()).map(Serial::new),
            product: d.product,
            location_id: d.location_id,
        })
        .collect();
    dfu_devices.sort_by_key(|d| (d.location_id, d.vid, d.pid));
    DeviceSnapshot { dfu_devices }
}

/// Sends a snapshot on every poll, changed or not: the machine counts
/// consecutive equal snapshots (`STABLE_POLLS`), so suppressing repeats
/// would stall it. Returns when the receiver is dropped.
pub async fn watch(tx: Sender<DeviceSnapshot>) {
    loop {
        let snap = tokio::task::spawn_blocking(|| match nusb::list_devices().wait() {
            Ok(devs) => snapshot_from(devs.map(|d| UsbDevice::from(&d))),
            // Enumeration hiccups read as "nothing attached"; the next poll
            // corrects it and the debounce absorbs the blip.
            Err(_) => DeviceSnapshot::default(),
        })
        .await
        .unwrap_or_default();
        if tx.send(snap).await.is_err() {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(vid: u16, pid: u16, serial: Option<&str>, interfaces: &[(u8, u8)]) -> UsbDevice {
        UsbDevice {
            vid,
            pid,
            serial: serial.map(str::to_string),
            product: Some("STM32  BOOTLOADER".into()),
            location_id: 0x0110_0000,
            interfaces: interfaces.to_vec(),
        }
    }

    #[test]
    fn keeps_only_dfu_interfaces_from_any_vid() {
        let snap = snapshot_from([
            dev(0x0483, 0xdf11, Some("A"), &[(0xfe, 0x01)]),
            dev(0x05ac, 0x8262, None, &[(0x03, 0x01)]),
            dev(0x1209, 0x0001, Some("B"), &[(0x02, 0x02), (0xfe, 0x01)]),
            // Application-specific class but not DFU (IrDA bridge).
            dev(0x1111, 0x2222, None, &[(0xfe, 0x02)]),
        ]);
        let ids: Vec<_> = snap.dfu_devices.iter().map(|d| (d.vid, d.pid)).collect();
        assert_eq!(ids, [(0x0483, 0xdf11), (0x1209, 0x0001)]);
    }

    #[test]
    fn carries_serial_product_and_location() {
        let snap = snapshot_from([dev(0x0483, 0xdf11, Some("3276365A3036"), &[(0xfe, 0x01)])]);
        assert_eq!(
            snap.dfu_devices,
            [DfuDeviceInfo {
                vid: 0x0483,
                pid: 0xdf11,
                serial: Some(Serial::new("3276365A3036")),
                product: Some("STM32  BOOTLOADER".into()),
                location_id: 0x0110_0000,
            }]
        );
    }

    #[test]
    fn blank_serial_counts_as_none() {
        let snap = snapshot_from([dev(0x0483, 0xdf11, Some("  "), &[(0xfe, 0x01)])]);
        assert_eq!(snap.dfu_devices[0].serial, None);
    }

    #[test]
    fn order_is_stable_across_polls() {
        let a = dev(0x0483, 0xdf11, Some("A"), &[(0xfe, 0x01)]);
        let mut b = dev(0x0483, 0xdf11, Some("B"), &[(0xfe, 0x01)]);
        b.location_id = 0x0120_0000;
        assert_eq!(snapshot_from([a.clone(), b.clone()]), snapshot_from([b, a]));
    }
}
