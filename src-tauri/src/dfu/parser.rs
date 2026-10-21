//! Pure classification of dfu-util 0.11 output (R8, R16, R21, R27, Error
//! mapping table). Patterns are case-insensitive substrings.

use crate::dfu::args::Op;
use crate::types::ReasonCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressOp {
    Upload,
    Download,
    Erase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Milestone {
    UploadDone,
    DownloadDone,
    LeaveSubmitted,
    DeviceOpened,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenignKind {
    /// `Invalid DFU suffix signature` and its follow-up warning.
    DfuSuffix,
    /// The first-contact `dfuERROR` quirk that dfu-util clears itself.
    FirstContactError,
    /// The device left DFU after `Download done.` (R8, R21).
    PostLeave,
}

/// What an error line means. Most map to a `ReasonCode`; three table rows
/// lead to their own states instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Reason(ReasonCode),
    /// The device vanished during backup or list: back to `waiting` (R21).
    NoDevice,
    Conflict,
    Unsupported,
    /// `Cannot open file` / `File exists`: a PocketBoot bug, since R19 gives
    /// every read a fresh path. Shown with the internal-error copy.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DfuEvent {
    Progress { op: ProgressOp, percent: u8 },
    Milestone(Milestone),
    Benign(BenignKind),
    Error(Failure, String),
    Other,
}

/// Parser context: which invocation is running and whether `Download done.`
/// has been seen yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    pub op: Op,
    pub download_done: bool,
}

/// Everything about a run that `final_outcome` needs, folded from events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    pub upload_done: bool,
    pub download_done: bool,
    pub leave_submitted: bool,
    pub post_leave_benign: bool,
    /// The first error line, which is the cause; later ones are fallout.
    pub first_error: Option<Failure>,
}

impl Seen {
    pub fn observe(&mut self, ev: &DfuEvent) {
        match ev {
            DfuEvent::Milestone(Milestone::UploadDone) => self.upload_done = true,
            DfuEvent::Milestone(Milestone::DownloadDone) => self.download_done = true,
            DfuEvent::Milestone(Milestone::LeaveSubmitted) => self.leave_submitted = true,
            DfuEvent::Benign(BenignKind::PostLeave) => self.post_leave_benign = true,
            DfuEvent::Error(f, _) => {
                self.first_error.get_or_insert(*f);
            }
            _ => {}
        }
    }

    /// The context for classifying the next line.
    pub fn ctx(&self, op: Op) -> Ctx {
        Ctx {
            op,
            download_done: self.download_done,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfuOutcome {
    Ok,
    /// Boot reached `Download done.` and then the leave request or a benign
    /// post-leave error: start the R8 watch window.
    SuccessCandidate,
    Failed(Failure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashLayout {
    pub base: u32,
    /// `(count, size_kb)` per segment group, e.g. `(4, 16)` for `04*016K`.
    pub segments: Vec<(u32, u32)>,
}

/// One `Found DFU:` line from `dfu-util -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundAlt {
    pub alt: u8,
    pub name: String,
    pub serial: Option<String>,
}

/// Splits on both `\n` and `\r` (progress redraws), dropping empty pieces.
pub fn split_lines(chunk: &str) -> impl Iterator<Item = &str> {
    chunk.split(['\r', '\n']).filter(|l| !l.trim().is_empty())
}

pub fn classify(line: &str, ctx: Ctx) -> DfuEvent {
    let l = line.to_ascii_lowercase();
    let has = |p: &str| l.contains(p);
    let any = |ps: &[&str]| ps.iter().any(|p| l.contains(p));
    let error = |f| DfuEvent::Error(f, line.to_string());
    let boot = ctx.op == Op::Download;

    if any(&[
        "invalid dfu suffix signature",
        "a valid dfu suffix will be required",
    ]) {
        return DfuEvent::Benign(BenignKind::DfuSuffix);
    }
    if any(&["state(10) = dfuerror", "firmware is corrupt"]) {
        return DfuEvent::Benign(BenignKind::FirstContactError);
    }
    if has("upload done.") {
        return DfuEvent::Milestone(Milestone::UploadDone);
    }
    if has("download done.") {
        return DfuEvent::Milestone(Milestone::DownloadDone);
    }
    if has("submitting leave request") {
        return DfuEvent::Milestone(Milestone::LeaveSubmitted);
    }
    if has("opening dfu capable usb device") {
        return DfuEvent::Milestone(Milestone::DeviceOpened);
    }
    // Ordered before the device-gone rows: after `Download done.` the radio
    // leaving DFU is the expected result, not a failure (R8, R21).
    if boot
        && ctx.download_done
        && any(&[
            "error during download get_status",
            "libusb_error_pipe",
            "libusb_error_no_device",
            "can't detach",
            "no dfu capable usb device available",
        ])
    {
        return DfuEvent::Benign(BenignKind::PostLeave);
    }
    if has("more than one dfu capable usb device found") {
        return error(Failure::Conflict);
    }
    if any(&[
        "no dfu capable usb device available",
        "libusb_error_no_device",
    ]) {
        return error(if boot {
            Failure::Reason(ReasonCode::DeviceGone)
        } else {
            Failure::NoDevice
        });
    }
    if any(&[
        "cannot open dfu device",
        "libusb_error_access",
        "libusb_error_busy",
    ]) {
        return error(Failure::Reason(ReasonCode::Busy));
    }
    if has("last page at") && has("is not writeable") {
        return error(Failure::Reason(ReasonCode::NotWriteable));
    }
    if any(&["cannot set alternate interface", "could not find alt"]) {
        return error(Failure::Unsupported);
    }
    if any(&[
        "libusb_error_io",
        "libusb_error_timeout",
        "error during upload",
        "error during download",
    ]) {
        return error(Failure::Reason(ReasonCode::UsbIo));
    }
    if any(&["cannot open file", "file exists"]) {
        return error(Failure::Internal);
    }
    progress(&l).unwrap_or(DfuEvent::Other)
}

/// `Upload\t[====   ]  45%   7372 bytes` and the Download/Erase forms.
fn progress(lower: &str) -> Option<DfuEvent> {
    let t = lower.trim_start();
    let op = if t.starts_with("upload") {
        ProgressOp::Upload
    } else if t.starts_with("download") {
        ProgressOp::Download
    } else if t.starts_with("erase") {
        ProgressOp::Erase
    } else {
        return None;
    };
    let before = t[..t.find('%')?].trim_end();
    let start = before
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let percent: u8 = before[start..].parse().ok()?;
    Some(DfuEvent::Progress {
        op,
        percent: percent.min(100),
    })
}

/// Combines the exit code with the milestones seen. The exit code alone never
/// decides a boot (R8): a benign post-leave error exits non-zero.
pub fn final_outcome(op: Op, exit_code: Option<i32>, seen: &Seen) -> DfuOutcome {
    let failed = || {
        DfuOutcome::Failed(
            seen.first_error
                .unwrap_or(Failure::Reason(ReasonCode::Unknown)),
        )
    };
    match op {
        Op::Version if exit_code == Some(0) => DfuOutcome::Ok,
        Op::Version => DfuOutcome::Failed(Failure::Reason(ReasonCode::SidecarMissing)),
        Op::Download if seen.download_done && (seen.leave_submitted || seen.post_leave_benign) => {
            DfuOutcome::SuccessCandidate
        }
        Op::Download => failed(),
        _ if seen.first_error.is_some() => failed(),
        Op::Upload if exit_code == Some(0) && seen.upload_done => DfuOutcome::Ok,
        Op::List if exit_code == Some(0) => DfuOutcome::Ok,
        Op::Upload | Op::List => failed(),
    }
}

/// R16: alt 0 must be `@Internal Flash` at `/0x08000000/` whose first
/// segment group is `04*016K`.
pub fn parse_layout(alt0_name: &str) -> Result<FlashLayout, String> {
    let bad = |why: &str| format!("{why}: {alt0_name:?}");
    let rest = alt0_name
        .strip_prefix("@Internal Flash")
        .ok_or_else(|| bad("not internal flash"))?
        .trim_start()
        .strip_prefix("/0x08000000/")
        .ok_or_else(|| bad("base is not 0x08000000"))?;
    let group = rest.split('/').next().unwrap_or_default();
    let segments = group
        .split(',')
        .map(|part| {
            let (count, size) = part.split_once('*')?;
            let digits = size.find(|c: char| !c.is_ascii_digit())?;
            let kb = match size[digits..].chars().next()? {
                'K' => size[..digits].parse().ok()?,
                'M' => size[..digits].parse::<u32>().ok()? * 1024,
                _ => return None,
            };
            Some((count.parse().ok()?, kb))
        })
        .collect::<Option<Vec<(u32, u32)>>>()
        .ok_or_else(|| bad("unparseable segments"))?;
    if segments.first() != Some(&(4, 16)) {
        return Err(bad("first segment is not 04*016K"));
    }
    Ok(FlashLayout {
        base: crate::dfu::args::ADDR,
        segments,
    })
}

/// Parses a `Found DFU: [..] ..., alt=N, name="..", serial=".."` line.
pub fn parse_found(line: &str) -> Option<FoundAlt> {
    let rest = line.trim_start().strip_prefix("Found DFU:")?;
    let quoted = |key: &str| {
        let start = rest.find(key)? + key.len();
        let end = rest[start..].find('"')?;
        Some(rest[start..start + end].to_string())
    };
    let alt_at = rest.find("alt=")? + 4;
    let alt_len = rest[alt_at..]
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len() - alt_at);
    let serial = quoted("serial=\"").filter(|s| !s.is_empty() && s != "UNKNOWN");
    Some(FoundAlt {
        alt: rest[alt_at..alt_at + alt_len].parse().ok()?,
        name: quoted("name=\"")?,
        serial,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReasonCode::*;

    const UP: Ctx = Ctx {
        op: Op::Upload,
        download_done: false,
    };
    const LIST: Ctx = Ctx {
        op: Op::List,
        download_done: false,
    };
    const DL: Ctx = Ctx {
        op: Op::Download,
        download_done: false,
    };
    const DL_DONE: Ctx = Ctx {
        op: Op::Download,
        download_done: true,
    };

    fn err(f: Failure) -> impl Fn(&DfuEvent) -> bool {
        move |e| matches!(e, DfuEvent::Error(got, _) if *got == f)
    }

    fn is(expected: DfuEvent) -> impl Fn(&DfuEvent) -> bool {
        move |e| *e == expected
    }

    /// One case per Error mapping table row (and per alternative pattern).
    #[test]
    #[allow(clippy::type_complexity)]
    fn error_mapping_table() {
        let benign = |k| is(DfuEvent::Benign(k));
        let ms = |m| is(DfuEvent::Milestone(m));
        let cases: Vec<(&str, Ctx, Box<dyn Fn(&DfuEvent) -> bool>)> = vec![
            ("dfu-util: Invalid DFU suffix signature", UP, Box::new(benign(BenignKind::DfuSuffix))),
            ("dfu-util: A valid DFU suffix will be required in a future dfu-util release", DL, Box::new(benign(BenignKind::DfuSuffix))),
            ("DFU state(10) = dfuERROR, status(10) = Device's firmware is corrupt.", UP, Box::new(benign(BenignKind::FirstContactError))),
            ("firmware is corrupt", LIST, Box::new(benign(BenignKind::FirstContactError))),
            ("Upload done.", UP, Box::new(ms(Milestone::UploadDone))),
            ("Download done.", DL, Box::new(ms(Milestone::DownloadDone))),
            ("Submitting leave request...", DL_DONE, Box::new(ms(Milestone::LeaveSubmitted))),
            ("Opening DFU capable USB device...", UP, Box::new(ms(Milestone::DeviceOpened))),
            ("dfu-util: Error during download get_status", DL_DONE, Box::new(benign(BenignKind::PostLeave))),
            ("LIBUSB_ERROR_PIPE", DL_DONE, Box::new(benign(BenignKind::PostLeave))),
            ("LIBUSB_ERROR_NO_DEVICE", DL_DONE, Box::new(benign(BenignKind::PostLeave))),
            ("dfu-util: can't detach", DL_DONE, Box::new(benign(BenignKind::PostLeave))),
            ("dfu-util: No DFU capable USB device available", UP, Box::new(err(Failure::NoDevice))),
            ("dfu-util: No DFU capable USB device available", LIST, Box::new(err(Failure::NoDevice))),
            ("dfu-util: Error during upload: LIBUSB_ERROR_NO_DEVICE", UP, Box::new(err(Failure::NoDevice))),
            ("dfu-util: No DFU capable USB device available", DL, Box::new(err(Failure::Reason(DeviceGone)))),
            ("dfu-util: Error during download: LIBUSB_ERROR_NO_DEVICE", DL, Box::new(err(Failure::Reason(DeviceGone)))),
            ("dfu-util: More than one DFU capable USB device found! Try `--list'", UP, Box::new(err(Failure::Conflict))),
            ("dfu-util: Cannot open DFU device 0483:df11 found on devnum 5 (LIBUSB_ERROR_ACCESS)", UP, Box::new(err(Failure::Reason(Busy)))),
            ("LIBUSB_ERROR_ACCESS", DL, Box::new(err(Failure::Reason(Busy)))),
            ("LIBUSB_ERROR_BUSY", LIST, Box::new(err(Failure::Reason(Busy)))),
            ("dfu-util: Last page at 0x08003fff is not writeable", DL, Box::new(err(Failure::Reason(NotWriteable)))),
            ("dfu-util: Cannot set alternate interface", UP, Box::new(err(Failure::Unsupported))),
            ("dfu-util: Could not find alt 0", DL, Box::new(err(Failure::Unsupported))),
            ("LIBUSB_ERROR_IO", UP, Box::new(err(Failure::Reason(UsbIo)))),
            ("LIBUSB_ERROR_TIMEOUT", DL, Box::new(err(Failure::Reason(UsbIo)))),
            ("dfu-util: Error during upload: LIBUSB_ERROR_OTHER", UP, Box::new(err(Failure::Reason(UsbIo)))),
            ("dfu-util: Error during download get_status", DL, Box::new(err(Failure::Reason(UsbIo)))),
            ("dfu-util: Cannot open file /x.partial for writing", UP, Box::new(err(Failure::Internal))),
            ("dfu-util: File exists", UP, Box::new(err(Failure::Internal))),
            ("Device returned transfer size 2048", UP, Box::new(is(DfuEvent::Other))),
        ];
        for (line, ctx, check) in cases {
            let got = classify(line, ctx);
            assert!(check(&got), "{line:?} in {ctx:?} -> {got:?}");
            // R27: case-insensitive.
            let upper = classify(&line.to_uppercase(), ctx);
            assert_eq!(
                std::mem::discriminant(&upper),
                std::mem::discriminant(&got),
                "{line:?}"
            );
        }
    }

    #[test]
    fn error_keeps_raw_line() {
        let line = "dfu-util: Cannot open DFU device 0483:df11 (LIBUSB_ERROR_BUSY)";
        assert_eq!(
            classify(line, UP),
            DfuEvent::Error(Failure::Reason(Busy), line.to_string())
        );
    }

    #[test]
    fn progress_on_cr_splits() {
        let chunk = "Upload\t[         ]   0%     0 bytes\rUpload\t[====     ]  45%  7372 bytes\rUpload\t[=========] 100% 16384 bytes\nUpload done.\n";
        let events: Vec<_> = split_lines(chunk).map(|l| classify(l, UP)).collect();
        let p = |percent| DfuEvent::Progress {
            op: ProgressOp::Upload,
            percent,
        };
        assert_eq!(
            events,
            [
                p(0),
                p(45),
                p(100),
                DfuEvent::Milestone(Milestone::UploadDone)
            ]
        );
        assert_eq!(
            classify("Erase   \t[====] 100%  16384 bytes", DL),
            DfuEvent::Progress {
                op: ProgressOp::Erase,
                percent: 100
            }
        );
        assert_eq!(
            classify("Download\t[==   ]  30%  4915 bytes", DL),
            DfuEvent::Progress {
                op: ProgressOp::Download,
                percent: 30
            }
        );
        assert_eq!(split_lines("\r\n\n").count(), 0);
    }

    /// Replays a fixture through split → classify → observe → final_outcome.
    fn replay(text: &str, op: Op) -> (Seen, DfuOutcome) {
        let (head, body) = text.split_once('\n').unwrap();
        let exit: i32 = head.strip_prefix("# exit: ").unwrap().parse().unwrap();
        let mut seen = Seen::default();
        for line in split_lines(body) {
            let ev = classify(line, seen.ctx(op));
            seen.observe(&ev);
        }
        let out = final_outcome(op, Some(exit), &seen);
        (seen, out)
    }

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../tests/fixtures/", $name))
        };
    }

    #[test]
    fn fixture_happy_upload() {
        let (seen, out) = replay(fixture!("happy-upload.txt"), Op::Upload);
        assert!(seen.upload_done);
        assert_eq!(out, DfuOutcome::Ok);
    }

    #[test]
    fn fixture_happy_download_leave_succeeds_despite_exit_74() {
        let (seen, out) = replay(fixture!("happy-download-leave.txt"), Op::Download);
        assert!(seen.download_done && seen.leave_submitted && seen.post_leave_benign);
        assert_eq!(seen.first_error, None);
        assert_eq!(out, DfuOutcome::SuccessCandidate);
    }

    #[test]
    fn fixture_first_contact_dfuerror_is_benign() {
        let (seen, out) = replay(fixture!("first-contact-dfuerror.txt"), Op::Upload);
        assert_eq!(seen.first_error, None);
        assert_eq!(out, DfuOutcome::Ok);
    }

    #[test]
    fn fixture_unplug_mid_upload_is_no_device() {
        let (_, out) = replay(fixture!("unplug-mid-upload.txt"), Op::Upload);
        assert_eq!(out, DfuOutcome::Failed(Failure::NoDevice));
    }

    #[test]
    fn fixture_unplug_mid_download_is_device_gone() {
        let (seen, out) = replay(fixture!("unplug-mid-download.txt"), Op::Download);
        assert!(!seen.download_done);
        assert_eq!(out, DfuOutcome::Failed(Failure::Reason(DeviceGone)));
    }

    #[test]
    fn fixture_busy_device() {
        let (_, out) = replay(fixture!("busy-device.txt"), Op::Upload);
        assert_eq!(out, DfuOutcome::Failed(Failure::Reason(Busy)));
    }

    #[test]
    fn final_outcome_rules() {
        let done = Seen {
            download_done: true,
            ..Seen::default()
        };
        // Download done but no leave and no benign error: not a success (R8).
        assert_eq!(
            final_outcome(Op::Download, Some(0), &done),
            DfuOutcome::Failed(Failure::Reason(Unknown))
        );
        let leave = Seen {
            leave_submitted: true,
            ..done.clone()
        };
        assert_eq!(
            final_outcome(Op::Download, Some(0), &leave),
            DfuOutcome::SuccessCandidate
        );
        // Upload done but non-zero exit is not trusted.
        let up = Seen {
            upload_done: true,
            ..Seen::default()
        };
        assert_eq!(
            final_outcome(Op::Upload, Some(74), &up),
            DfuOutcome::Failed(Failure::Reason(Unknown))
        );
        assert_eq!(final_outcome(Op::Upload, Some(0), &up), DfuOutcome::Ok);
        assert_eq!(
            final_outcome(Op::Upload, Some(0), &Seen::default()),
            DfuOutcome::Failed(Failure::Reason(Unknown))
        );
        assert_eq!(
            final_outcome(Op::List, Some(0), &Seen::default()),
            DfuOutcome::Ok
        );
        assert_eq!(
            final_outcome(Op::Version, Some(0), &Seen::default()),
            DfuOutcome::Ok
        );
        assert_eq!(
            final_outcome(Op::Version, Some(64), &Seen::default()),
            DfuOutcome::Failed(Failure::Reason(SidecarMissing))
        );
        // Killed by a signal (no exit code) with nothing seen.
        assert_eq!(
            final_outcome(Op::List, None, &Seen::default()),
            DfuOutcome::Failed(Failure::Reason(Unknown))
        );
    }

    #[test]
    fn seen_keeps_first_error() {
        let mut s = Seen::default();
        s.observe(&DfuEvent::Error(Failure::Reason(UsbIo), String::new()));
        s.observe(&DfuEvent::Error(Failure::Reason(Busy), String::new()));
        assert_eq!(s.first_error, Some(Failure::Reason(UsbIo)));
    }

    #[test]
    fn layout_accepts_pocket() {
        let l = parse_layout("@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg").unwrap();
        assert_eq!(l.base, 0x0800_0000);
        assert_eq!(l.segments, [(4, 16), (1, 64), (7, 128)]);
    }

    #[test]
    fn layout_rejects_others() {
        for name in [
            "@Internal Flash   /0x08000000/16*128Kg",
            "@Internal Flash  /0x08000000/01*128Kg,04*016Kg",
            "@Internal Flash  /0x08004000/04*016Kg,01*064Kg",
            "@Option Bytes  /0x1FFFC000/01*016 e",
            "@Internal Flash  /0x08000000/garbage",
            "",
        ] {
            assert!(parse_layout(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn found_line() {
        let line = r#"Found DFU: [0483:df11] ver=2200, devnum=5, cfg=1, intf=0, path="1-1", alt=0, name="@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg", serial="3276365A3036""#;
        assert_eq!(
            parse_found(line),
            Some(FoundAlt {
                alt: 0,
                name: "@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg".into(),
                serial: Some("3276365A3036".into()),
            })
        );
        let no_serial = r#"Found DFU: [0483:df11] ver=2200, devnum=5, cfg=1, intf=0, path="1-1", alt=1, name="@Option Bytes  /0x1FFFC000/01*016 e", serial="UNKNOWN""#;
        assert_eq!(parse_found(no_serial).unwrap().serial, None);
        assert_eq!(parse_found(no_serial).unwrap().alt, 1);
        assert_eq!(parse_found("Opening DFU capable USB device..."), None);
    }
}
