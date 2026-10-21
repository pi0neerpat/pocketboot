use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::types::{ReasonCode, Serial};

/// Sector 0 of the STM32F4 is exactly 16 KB (R10(b), R16).
pub const SECTOR0_LEN: usize = 16384;
/// Backups kept per serial, not counting the never-pruned `-first` file (R20).
pub const RETAIN: usize = 20;
/// Q8 vector-table sanity check. Off until real Q5 captures confirm the
/// ranges (KTD9); enabling it is a human Definition-of-Done item.
pub const VECTOR_CHECK: bool = false;

const FIRST_SUFFIX: &str = "-sector0-first.bin";
const NORMAL_SUFFIX: &str = "-sector0.bin";

/// A not-yet-validated upload target. Only `BackupManager::new_partial`
/// creates one, so no caller can point dfu-util at an arbitrary path (R15).
#[derive(Debug)]
pub struct PartialPath(PathBuf);

impl PartialPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A pointer to a finalized backup file plus the hash it had when validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRef {
    pub path: PathBuf,
    pub sha256: [u8; 32],
    /// RFC 3339 UTC timestamp, used for the `backupAt` copy.
    pub created_at: String,
}

/// A backup that passed R10(a)–(d). The fields are private and only
/// `validate` / `revalidate` in this module build one (KTD2), so a write can
/// never be issued from an unchecked image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedBackup {
    r: BackupRef,
}

impl ValidatedBackup {
    pub fn path(&self) -> &Path {
        &self.r.path
    }

    pub fn backup_ref(&self) -> &BackupRef {
        &self.r
    }

    /// Test-only constructor for state-machine tests that never touch disk.
    #[cfg(test)]
    pub(crate) fn for_test(r: BackupRef) -> Self {
        Self { r }
    }
}

/// How a fresh read relates to the first valid backup of this run (R14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareResult {
    /// No earlier backup for this serial in this run.
    NoEarlier,
    Same,
    Differs,
    /// The fresh read is not 16384 bytes, so it cannot be compared.
    Incomparable,
}

/// Fields for the `.json` sidecar beyond what the manager computes (R18).
#[derive(Debug, Clone)]
pub struct BackupMeta {
    pub serial: Option<Serial>,
    pub app_version: String,
    pub dfu_util_version: String,
    pub layout: String,
}

#[derive(Serialize)]
struct SidecarJson<'a> {
    serial: Option<&'a str>,
    size: usize,
    sha256: String,
    created_at: &'a str,
    app_version: &'a str,
    dfu_util_version: &'a str,
    layout: &'a str,
}

pub struct BackupManager {
    root: PathBuf,
    vector_check: bool,
}

impl BackupManager {
    /// `root` is `<app_data_dir>/backups`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            vector_check: VECTOR_CHECK,
        }
    }

    #[cfg(test)]
    fn with_vector_check(mut self, on: bool) -> Self {
        self.vector_check = on;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn serial_dir(&self, serial: Option<&Serial>) -> PathBuf {
        self.root.join(sanitize_serial(serial))
    }

    /// The folder `Show backups` opens, created if absent.
    pub fn reveal_dir(&self) -> std::io::Result<PathBuf> {
        ensure_private_dir(&self.root)?;
        Ok(self.root.clone())
    }

    /// Reserves a fresh `.partial` path. The file itself is not created:
    /// dfu-util opens `-U` targets create-exclusive (R19).
    pub fn new_partial(&self, serial: Option<&Serial>) -> std::io::Result<PartialPath> {
        ensure_private_dir(&self.root)?;
        let dir = self.serial_dir(serial);
        ensure_private_dir(&dir)?;
        let ts = time::OffsetDateTime::now_utc()
            .format(time::macros::format_description!(
                "[year][month][day]T[hour][minute][second]Z"
            ))
            .map_err(std::io::Error::other)?;
        for n in 0u32.. {
            let stem = if n == 0 {
                ts.clone()
            } else {
                format!("{ts}-{n}")
            };
            let taken = [
                format!("{stem}{NORMAL_SUFFIX}.partial"),
                format!("{stem}{NORMAL_SUFFIX}"),
                format!("{stem}{FIRST_SUFFIX}"),
            ]
            .iter()
            .any(|name| dir.join(name).exists());
            if !taken {
                return Ok(PartialPath(
                    dir.join(format!("{stem}{NORMAL_SUFFIX}.partial")),
                ));
            }
        }
        unreachable!("u32 suffixes exhausted")
    }

    /// R10: upload completed, exactly 16384 bytes, not blank, optional vector
    /// check. A failed partial is deleted (R19).
    pub fn validate(
        &self,
        partial: PartialPath,
        upload_done: bool,
    ) -> Result<ValidatedBackup, ReasonCode> {
        let checked = if upload_done {
            fs::read(&partial.0)
                .map_err(|_| ReasonCode::Unknown)
                .and_then(|bytes| self.check_image(&bytes).map(|()| sha256(&bytes)))
        } else {
            Err(ReasonCode::Unknown)
        };
        match checked {
            Ok(sha256) => Ok(ValidatedBackup {
                r: BackupRef {
                    path: partial.0,
                    sha256,
                    created_at: now_rfc3339(),
                },
            }),
            Err(code) => {
                self.discard(partial);
                Err(code)
            }
        }
    }

    /// R14 comparison against the first valid backup of this run. Runs on the
    /// raw read, before the blank checks, so a blanked sector reads as changed.
    pub fn compare(&self, partial: &PartialPath, earlier: Option<&BackupRef>) -> CompareResult {
        let Some(earlier) = earlier else {
            return CompareResult::NoEarlier;
        };
        match fs::read(&partial.0) {
            Ok(bytes) if bytes.len() == SECTOR0_LEN => {
                if sha256(&bytes) == earlier.sha256 {
                    CompareResult::Same
                } else {
                    CompareResult::Differs
                }
            }
            _ => CompareResult::Incomparable,
        }
    }

    pub fn discard(&self, partial: PartialPath) {
        let _ = fs::remove_file(&partial.0);
    }

    /// Renames the validated partial to its final name, writes the JSON
    /// sidecar, and prunes (R18–R20).
    pub fn finalize(
        &self,
        v: ValidatedBackup,
        meta: &BackupMeta,
    ) -> std::io::Result<ValidatedBackup> {
        let dir =
            v.r.path
                .parent()
                .ok_or_else(|| std::io::Error::other("partial has no parent"))?
                .to_path_buf();
        let name =
            v.r.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
        let stem = name
            .strip_suffix(&format!("{NORMAL_SUFFIX}.partial"))
            .ok_or_else(|| std::io::Error::other("not a partial path"))?;
        let is_first = !list_bins(&dir)?
            .iter()
            .any(|(n, _)| n.ends_with(FIRST_SUFFIX));
        let suffix = if is_first {
            FIRST_SUFFIX
        } else {
            NORMAL_SUFFIX
        };
        let final_path = dir.join(format!("{stem}{suffix}"));

        fs::rename(&v.r.path, &final_path)?;
        set_mode(&final_path, 0o600)?;

        let json = SidecarJson {
            serial: meta.serial.as_ref().map(Serial::as_str),
            size: SECTOR0_LEN,
            sha256: hex(&v.r.sha256),
            created_at: &v.r.created_at,
            app_version: &meta.app_version,
            dfu_util_version: &meta.dfu_util_version,
            layout: &meta.layout,
        };
        let json_path = final_path.with_extension("json");
        fs::write(&json_path, serde_json::to_vec_pretty(&json)?)?;
        set_mode(&json_path, 0o600)?;

        prune(&dir)?;
        Ok(ValidatedBackup {
            r: BackupRef {
                path: final_path,
                ..v.r
            },
        })
    }

    /// Re-hashes the file immediately before a write (R11).
    pub fn recheck(&self, v: &ValidatedBackup) -> Result<(), ReasonCode> {
        match fs::read(&v.r.path) {
            Ok(bytes) if sha256(&bytes) == v.r.sha256 => Ok(()),
            _ => Err(ReasonCode::HashMismatch),
        }
    }

    /// Restore path: re-reads a stored backup and re-runs R10(b)–(e) (KTD2).
    pub fn revalidate(&self, r: &BackupRef) -> Result<ValidatedBackup, ReasonCode> {
        let bytes = fs::read(&r.path).map_err(|_| ReasonCode::HashMismatch)?;
        self.check_image(&bytes)?;
        if sha256(&bytes) != r.sha256 {
            return Err(ReasonCode::HashMismatch);
        }
        Ok(ValidatedBackup { r: r.clone() })
    }

    /// Backup count and newest filename for a serial, for diagnostics.
    pub fn stats(&self, serial: Option<&Serial>) -> (usize, Option<String>) {
        let bins = list_bins(&self.serial_dir(serial)).unwrap_or_default();
        let newest = bins
            .iter()
            .max_by_key(|(_, k)| k.clone())
            .map(|(n, _)| n.clone());
        (bins.len(), newest)
    }

    fn check_image(&self, bytes: &[u8]) -> Result<(), ReasonCode> {
        if bytes.len() != SECTOR0_LEN {
            return Err(ReasonCode::ValidationSize);
        }
        if bytes.iter().all(|&b| b == 0xFF) || bytes.iter().all(|&b| b == 0x00) {
            return Err(ReasonCode::ValidationBlank);
        }
        if self.vector_check && !vectors_plausible(bytes) {
            return Err(ReasonCode::ValidationBlank);
        }
        Ok(())
    }
}

/// KTD9: initial SP in SRAM or CCM, reset vector in flash.
fn vectors_plausible(bytes: &[u8]) -> bool {
    let word = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let sp = word(0);
    let reset = word(4);
    let sp_ok =
        (0x2000_0000..0x2003_0000).contains(&sp) || (0x1000_0000..0x1001_0000).contains(&sp);
    sp_ok && reset >> 24 == 0x08
}

/// Final `.bin` files in `dir` with their `(timestamp, collision n)` sort key.
fn list_bins(dir: &Path) -> std::io::Result<Vec<(String, (String, u32))>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let Some(stem) = name
            .strip_suffix(FIRST_SUFFIX)
            .or_else(|| name.strip_suffix(NORMAL_SUFFIX))
        else {
            continue;
        };
        let (ts, n) = match stem.split_once('-') {
            Some((ts, n)) => (ts.to_string(), n.parse().unwrap_or(0)),
            None => (stem.to_string(), 0),
        };
        out.push((name, (ts, n)));
    }
    Ok(out)
}

/// R20: keep the newest `RETAIN` normal backups; `-first` is never pruned.
fn prune(dir: &Path) -> std::io::Result<()> {
    let mut normal: Vec<_> = list_bins(dir)?
        .into_iter()
        .filter(|(n, _)| !n.ends_with(FIRST_SUFFIX))
        .collect();
    normal.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, _) in normal.into_iter().skip(RETAIN) {
        let bin = dir.join(&name);
        fs::remove_file(&bin)?;
        let _ = fs::remove_file(bin.with_extension("json"));
    }
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    set_mode(dir, 0o700)
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// R18: `[A-Za-z0-9_-]` only; a missing or empty serial maps to `no-serial`.
pub fn sanitize_serial(serial: Option<&Serial>) -> String {
    match serial.map(Serial::as_str) {
        None | Some("") => "no-serial".to_string(),
        Some(s) => s
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect(),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mgr() -> (tempfile::TempDir, BackupManager) {
        let dir = tempfile::tempdir().unwrap();
        let m = BackupManager::new(dir.path().join("backups"));
        (dir, m)
    }

    fn serial() -> Serial {
        Serial::new("3276365A3036")
    }

    fn meta() -> BackupMeta {
        BackupMeta {
            serial: Some(serial()),
            app_version: "0.1.0".into(),
            dfu_util_version: "dfu-util 0.11".into(),
            layout: "04*016K".into(),
        }
    }

    /// Plausible sector 0: SP in SRAM, reset vector in flash, then noise.
    fn good_image(seed: u8) -> Vec<u8> {
        let mut v: Vec<u8> = (0..SECTOR0_LEN)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect();
        v[0..4].copy_from_slice(&0x2002_0000u32.to_le_bytes());
        v[4..8].copy_from_slice(&0x0800_01c1u32.to_le_bytes());
        v
    }

    fn upload(m: &BackupManager, bytes: &[u8]) -> PartialPath {
        let p = m.new_partial(Some(&serial())).unwrap();
        fs::write(p.as_path(), bytes).unwrap();
        p
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn sanitizes_serial() {
        assert_eq!(sanitize_serial(Some(&Serial::new("AB-12_x"))), "AB-12_x");
        assert_eq!(sanitize_serial(Some(&Serial::new("../a b"))), "___a_b");
        assert_eq!(sanitize_serial(Some(&Serial::new(""))), "no-serial");
        assert_eq!(sanitize_serial(None), "no-serial");
    }

    #[test]
    fn rejects_wrong_sizes() {
        let (_d, m) = mgr();
        for len in [SECTOR0_LEN - 1, SECTOR0_LEN + 1, 0] {
            let mut img = good_image(1);
            img.resize(len, 0x11);
            let p = upload(&m, &img);
            let path = p.as_path().to_path_buf();
            assert_eq!(m.validate(p, true), Err(ReasonCode::ValidationSize));
            assert!(!path.exists(), "failed partial must be deleted (R19)");
        }
    }

    #[test]
    fn rejects_blank_images() {
        let (_d, m) = mgr();
        for b in [0xFFu8, 0x00] {
            let p = upload(&m, &vec![b; SECTOR0_LEN]);
            assert_eq!(m.validate(p, true), Err(ReasonCode::ValidationBlank));
        }
    }

    #[test]
    fn rejects_without_upload_done() {
        let (_d, m) = mgr();
        let p = upload(&m, &good_image(1));
        assert_eq!(m.validate(p, false), Err(ReasonCode::Unknown));
    }

    #[test]
    fn accepts_random_image() {
        let (_d, m) = mgr();
        let img = good_image(7);
        let p = upload(&m, &img);
        let v = m.validate(p, true).unwrap();
        let want: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&img).into();
        assert_eq!(v.backup_ref().sha256, want);
    }

    #[test]
    fn vector_check_is_off_by_default_and_rejects_when_on() {
        let mut bad = good_image(3);
        bad[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes());

        let (_d, m) = mgr();
        assert!(!VECTOR_CHECK);
        assert!(m.validate(upload(&m, &bad), true).is_ok());

        let (_d2, m2) = mgr();
        let m2 = m2.with_vector_check(true);
        assert_eq!(
            m2.validate(upload(&m2, &bad), true),
            Err(ReasonCode::ValidationBlank)
        );
        assert!(m2.validate(upload(&m2, &good_image(3)), true).is_ok());

        let mut ccm = good_image(4);
        ccm[0..4].copy_from_slice(&0x1000_8000u32.to_le_bytes());
        assert!(m2.validate(upload(&m2, &ccm), true).is_ok());

        let mut bad_reset = good_image(5);
        bad_reset[4..8].copy_from_slice(&0x0900_0000u32.to_le_bytes());
        assert_eq!(
            m2.validate(upload(&m2, &bad_reset), true),
            Err(ReasonCode::ValidationBlank)
        );
    }

    #[test]
    fn compare_against_first_in_run() {
        let (_d, m) = mgr();
        let first = m.finalize(
            m.validate(upload(&m, &good_image(1)), true).unwrap(),
            &meta(),
        );
        let first = first.unwrap().backup_ref().clone();

        assert_eq!(
            m.compare(&upload(&m, &good_image(1)), None),
            CompareResult::NoEarlier
        );
        assert_eq!(
            m.compare(&upload(&m, &good_image(1)), Some(&first)),
            CompareResult::Same
        );
        assert_eq!(
            m.compare(&upload(&m, &good_image(2)), Some(&first)),
            CompareResult::Differs
        );
        // R14: a blank read is compared before the blank check, so it differs.
        assert_eq!(
            m.compare(&upload(&m, &vec![0xFF; SECTOR0_LEN]), Some(&first)),
            CompareResult::Differs
        );
        assert_eq!(
            m.compare(&upload(&m, &[1, 2, 3]), Some(&first)),
            CompareResult::Incomparable
        );
    }

    #[test]
    fn finalize_writes_bin_and_json_with_permissions() {
        let (_d, m) = mgr();
        let v = m.validate(upload(&m, &good_image(1)), true).unwrap();
        let f = m.finalize(v, &meta()).unwrap();
        let path = f.path().to_path_buf();
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.ends_with(FIRST_SUFFIX),
            "first backup is labelled: {name}"
        );
        assert_eq!(name.len(), "20261021T104700Z".len() + FIRST_SUFFIX.len());

        let json_path = path.with_extension("json");
        let j: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(j["size"], SECTOR0_LEN);
        assert_eq!(j["serial"], "3276365A3036");
        assert_eq!(j["sha256"], hex(&f.backup_ref().sha256));
        for k in ["created_at", "app_version", "dfu_util_version", "layout"] {
            assert!(j[k].is_string(), "{k}");
        }

        let dir = m.serial_dir(Some(&serial()));
        assert_eq!(mode(&m.root), 0o700);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&json_path), 0o600);

        let second = m.validate(upload(&m, &good_image(1)), true).unwrap();
        let second = m.finalize(second, &meta()).unwrap();
        let name2 = second.path().file_name().unwrap().to_str().unwrap();
        assert!(name2.ends_with(NORMAL_SUFFIX) && !name2.contains("first"));
    }

    #[test]
    fn names_are_unique_under_rapid_repeats() {
        let (_d, m) = mgr();
        let mut seen = std::collections::HashSet::new();
        for i in 0..8 {
            let v = m.validate(upload(&m, &good_image(i)), true).unwrap();
            let f = m.finalize(v, &meta()).unwrap();
            assert!(seen.insert(f.path().to_path_buf()), "duplicate name");
        }
        let partials = fs::read_dir(m.serial_dir(Some(&serial())))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".partial")
            })
            .count();
        assert_eq!(partials, 0);
    }

    #[test]
    fn prune_keeps_twenty_plus_first() {
        let (_d, m) = mgr();
        let mut first = None;
        for i in 0..(RETAIN as u8 + 6) {
            let v = m.validate(upload(&m, &good_image(i)), true).unwrap();
            let f = m.finalize(v, &meta()).unwrap();
            first.get_or_insert(f.path().to_path_buf());
        }
        let dir = m.serial_dir(Some(&serial()));
        let bins: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "bin"))
            .collect();
        let jsons = fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|e| e == "json")
            })
            .count();
        assert_eq!(bins.len(), RETAIN + 1);
        assert_eq!(jsons, RETAIN + 1);
        assert!(first.unwrap().exists(), "-first is never pruned");
        assert_eq!(m.stats(Some(&serial())).0, RETAIN + 1);
    }

    #[test]
    fn recheck_detects_tampering() {
        let (_d, m) = mgr();
        let v = m.validate(upload(&m, &good_image(1)), true).unwrap();
        let v = m.finalize(v, &meta()).unwrap();
        assert_eq!(m.recheck(&v), Ok(()));
        let mut bytes = fs::read(v.path()).unwrap();
        bytes[100] ^= 0xFF;
        fs::write(v.path(), &bytes).unwrap();
        assert_eq!(m.recheck(&v), Err(ReasonCode::HashMismatch));
        fs::remove_file(v.path()).unwrap();
        assert_eq!(m.recheck(&v), Err(ReasonCode::HashMismatch));
    }

    #[test]
    fn revalidate_accepts_intact_and_rejects_damaged() {
        let (_d, m) = mgr();
        let v = m.validate(upload(&m, &good_image(1)), true).unwrap();
        let v = m.finalize(v, &meta()).unwrap();
        let r = v.backup_ref().clone();
        assert_eq!(m.revalidate(&r), Ok(v.clone()));

        let bytes = fs::read(&r.path).unwrap();
        fs::write(&r.path, &bytes[..SECTOR0_LEN - 1]).unwrap();
        assert_eq!(m.revalidate(&r), Err(ReasonCode::ValidationSize));

        fs::write(&r.path, vec![0xFF; SECTOR0_LEN]).unwrap();
        assert_eq!(m.revalidate(&r), Err(ReasonCode::ValidationBlank));

        let mut other = good_image(9);
        other[200] = 0;
        fs::write(&r.path, &other).unwrap();
        assert_eq!(m.revalidate(&r), Err(ReasonCode::HashMismatch));
    }

    #[test]
    fn discard_removes_partial() {
        let (_d, m) = mgr();
        let p = upload(&m, &good_image(1));
        let path = p.as_path().to_path_buf();
        m.discard(p);
        assert!(!path.exists());
    }
}
