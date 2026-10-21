//! Sector-0 safety copies: paths, validation, hashing, retention (R10, R14, R18–R20).

pub mod manager;

pub use manager::{
    BackupManager, BackupMeta, BackupRef, CompareResult, PartialPath, ValidatedBackup,
};
