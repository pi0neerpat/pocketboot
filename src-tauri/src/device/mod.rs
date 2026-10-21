//! USB presence (R24) and the R16 flash-layout probe.

pub mod layout;
pub mod watcher;

pub use watcher::{snapshot_from, watch, UsbDevice};
