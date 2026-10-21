//! The only place dfu-util arguments are built (R12, R13, KTD10). No public
//! function takes an address, a length, or an arbitrary path.

use std::ffi::OsString;
use std::time::Duration;

use crate::backup::{PartialPath, ValidatedBackup};
use crate::types::Serial;

pub const VIDPID: &str = "0483:df11";
pub const ALT: &str = "0";
pub const ADDR: u32 = 0x0800_0000;
pub const LEN: usize = crate::backup::manager::SECTOR0_LEN;

/// Which dfu-util invocation is running; also the parser's context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Version,
    List,
    Upload,
    Download,
}

/// A complete, ready-to-spawn dfu-util call with its total timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub op: Op,
    pub argv: Vec<OsString>,
    pub timeout: Duration,
}

pub fn version() -> Invocation {
    Invocation {
        op: Op::Version,
        argv: vec!["-V".into()],
        timeout: Duration::from_secs(5),
    }
}

pub fn list() -> Invocation {
    Invocation {
        op: Op::List,
        argv: vec!["-l".into(), "-d".into(), VIDPID.into()],
        timeout: Duration::from_secs(5),
    }
}

/// `serial` is `None` only in the R13 no-serial fallback, where the caller
/// has just confirmed exactly one DFU device is present.
pub fn upload(serial: Option<&Serial>, dest: &PartialPath) -> Invocation {
    let mut argv = pinned(serial);
    argv.extend([
        "-s".into(),
        format!("{ADDR:#010x}:{LEN}").into(),
        "-U".into(),
        dest.as_path().into(),
    ]);
    Invocation {
        op: Op::Upload,
        argv,
        timeout: Duration::from_secs(20),
    }
}

pub fn download_leave(serial: Option<&Serial>, src: &ValidatedBackup) -> Invocation {
    let mut argv = pinned(serial);
    argv.extend([
        "-s".into(),
        format!("{ADDR:#010x}:leave").into(),
        "-D".into(),
        src.path().into(),
    ]);
    Invocation {
        op: Op::Download,
        argv,
        timeout: Duration::from_secs(30),
    }
}

/// `-d 0483:df11 [-S <serial>] -a 0`, shared by every transfer (R13).
fn pinned(serial: Option<&Serial>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["-d".into(), VIDPID.into()];
    if let Some(s) = serial {
        argv.extend(["-S".into(), s.as_str().into()]);
    }
    argv.extend(["-a".into(), ALT.into()]);
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::{BackupManager, BackupRef};
    use std::path::PathBuf;

    fn strs(inv: &Invocation) -> Vec<String> {
        inv.argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn assert_pinned(argv: &[String], serial: &str) {
        let pos = |flag: &str| argv.iter().position(|a| a == flag).expect(flag);
        assert_eq!(argv[pos("-d") + 1], VIDPID);
        assert_eq!(argv[pos("-S") + 1], serial);
        assert_eq!(argv[pos("-a") + 1], ALT);
        assert!(argv[pos("-s") + 1].starts_with("0x08000000:"));
    }

    #[test]
    fn version_and_list_are_fixed_vectors() {
        let v = version();
        assert_eq!(strs(&v), ["-V"]);
        assert_eq!((v.op, v.timeout), (Op::Version, Duration::from_secs(5)));
        let l = list();
        assert_eq!(strs(&l), ["-l", "-d", "0483:df11"]);
        assert_eq!((l.op, l.timeout), (Op::List, Duration::from_secs(5)));
    }

    #[test]
    fn upload_is_pinned_to_sector0() {
        let dir = tempfile::tempdir().unwrap();
        let m = BackupManager::new(dir.path());
        let s = Serial::new("3276365A3036");
        let p = m.new_partial(Some(&s)).unwrap();
        let inv = upload(Some(&s), &p);
        let argv = strs(&inv);
        assert_pinned(&argv, "3276365A3036");
        assert_eq!(
            &argv[6..],
            [
                "-s",
                "0x08000000:16384",
                "-U",
                &p.as_path().to_string_lossy()
            ]
        );
        assert_eq!((inv.op, inv.timeout), (Op::Upload, Duration::from_secs(20)));
    }

    #[test]
    fn download_leave_is_pinned_to_sector0() {
        let v = ValidatedBackup::for_test(BackupRef {
            path: PathBuf::from("/b/S/20261021T104700Z-sector0.bin"),
            sha256: [0; 32],
            created_at: String::new(),
        });
        let inv = download_leave(Some(&Serial::new("S")), &v);
        let argv = strs(&inv);
        assert_pinned(&argv, "S");
        assert_eq!(
            &argv[6..],
            [
                "-s",
                "0x08000000:leave",
                "-D",
                "/b/S/20261021T104700Z-sector0.bin"
            ]
        );
        assert_eq!(
            (inv.op, inv.timeout),
            (Op::Download, Duration::from_secs(30))
        );
    }

    #[test]
    fn no_serial_fallback_omits_only_the_serial() {
        let v = ValidatedBackup::for_test(BackupRef {
            path: PathBuf::from("/b/x.bin"),
            sha256: [0; 32],
            created_at: String::new(),
        });
        let argv = strs(&download_leave(None, &v));
        assert!(!argv.contains(&"-S".to_string()));
        assert_eq!(&argv[..4], ["-d", "0483:df11", "-a", "0"]);
    }
}
