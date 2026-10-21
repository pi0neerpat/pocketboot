//! R16: run `dfu-util -l` and read the alt-0 layout of the session's device.

use tokio::sync::mpsc;

use crate::dfu::parser::{parse_found, parse_layout};
use crate::dfu::runner::DfuRunner;
use crate::dfu::{args, DfuOutcome, Failure, FlashLayout};
use crate::types::Serial;

pub async fn probe(
    runner: &impl DfuRunner,
    serial: Option<&Serial>,
) -> Result<FlashLayout, Failure> {
    // The listing's log lines are only needed in `report.lines`.
    let (tx, _rx) = mpsc::unbounded_channel();
    let report = runner.run(args::list(), tx).await;
    match report.outcome {
        DfuOutcome::Failed(f) => Err(f),
        // `-l` never leaves DFU mode, so a success candidate is just a listing.
        DfuOutcome::Ok | DfuOutcome::SuccessCandidate => select(&report.lines, serial),
    }
}

/// Picks the alt-0 line for `serial` from `dfu-util -l` output.
/// Size checks on the parsed layout belong to the machine; only an
/// unparseable name is `Unsupported` here.
pub fn select(lines: &[String], serial: Option<&Serial>) -> Result<FlashLayout, Failure> {
    let alt0 = lines
        .iter()
        .filter_map(|l| parse_found(l))
        .find(|f| f.alt == 0 && serial.is_none_or(|s| f.serial.as_deref() == Some(s.as_str())))
        .ok_or(Failure::NoDevice)?;
    parse_layout(&alt0.name).map_err(|_| Failure::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dfu::runner::{RunMsg, RunReport};
    use crate::types::ReasonCode;

    const FLASH: &str = "@Internal Flash  /0x08000000/04*016Kg,01*064Kg,07*128Kg";

    fn found(alt: u8, name: &str, serial: &str) -> String {
        format!(
            r#"Found DFU: [0483:df11] ver=2200, devnum=5, cfg=1, intf=0, path="1-1", alt={alt}, name="{name}", serial="{serial}""#
        )
    }

    fn lines(ls: &[String]) -> Vec<String> {
        let mut v = vec!["dfu-util 0.11".to_string()];
        v.extend_from_slice(ls);
        v
    }

    #[test]
    fn selects_alt0_for_the_session_serial() {
        let out = lines(&[
            found(1, "@Option Bytes  /0x1FFFC000/01*016 e", "A"),
            found(0, "@Internal Flash  /0x08000000/02*016Kg", "B"),
            found(0, FLASH, "A"),
        ]);
        let l = select(&out, Some(&Serial::new("A"))).unwrap();
        assert_eq!(l.segments, [(4, 16), (1, 64), (7, 128)]);
    }

    #[test]
    fn missing_serial_is_no_device() {
        let out = lines(&[found(0, FLASH, "B")]);
        assert_eq!(
            select(&out, Some(&Serial::new("A"))),
            Err(Failure::NoDevice)
        );
    }

    #[test]
    fn no_serial_fallback_takes_the_unnamed_alt0() {
        let out = lines(&[found(0, FLASH, "UNKNOWN")]);
        assert!(select(&out, None).is_ok());
    }

    #[test]
    fn wrong_layout_is_unsupported() {
        let out = lines(&[found(0, "@Option Bytes  /0x1FFFC000/01*016 e", "A")]);
        assert_eq!(
            select(&out, Some(&Serial::new("A"))),
            Err(Failure::Unsupported)
        );
    }

    struct Canned(DfuOutcome, Vec<String>);

    impl DfuRunner for Canned {
        async fn run(
            &self,
            inv: args::Invocation,
            _tx: mpsc::UnboundedSender<RunMsg>,
        ) -> RunReport {
            assert_eq!(inv, args::list());
            RunReport {
                outcome: self.0,
                lines: self.1.clone(),
            }
        }
    }

    #[tokio::test]
    async fn probe_runs_list_and_selects() {
        let r = Canned(DfuOutcome::Ok, lines(&[found(0, FLASH, "A")]));
        assert!(probe(&r, Some(&Serial::new("A"))).await.is_ok());
    }

    #[tokio::test]
    async fn probe_passes_list_failures_through() {
        let busy = Failure::Reason(ReasonCode::Busy);
        let r = Canned(DfuOutcome::Failed(busy), vec![]);
        assert_eq!(probe(&r, Some(&Serial::new("A"))).await, Err(busy));
    }
}
