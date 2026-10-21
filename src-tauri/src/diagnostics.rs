//! The log ring behind `log-line` and `get_log` (R51), and the plain-text
//! report behind Copy diagnostics (R52).

use std::collections::VecDeque;
use std::path::Path;

use serde::Serialize;

use crate::dfu::FlashLayout;
use crate::types::DfuDeviceInfo;

/// Lines kept for the log view (R51).
pub const LOG_CAP: usize = 500;
/// Lines copied into a diagnostics report (R52).
pub const REPORT_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
    App,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogLine {
    pub ts: String,
    pub stream: LogStream,
    pub text: String,
}

impl LogLine {
    pub fn now(stream: LogStream, text: impl Into<String>) -> Self {
        Self {
            ts: crate::backup::manager::now_rfc3339(),
            stream,
            text: text.into(),
        }
    }
}

#[derive(Debug, Default)]
pub struct LogRing {
    lines: VecDeque<LogLine>,
}

impl LogRing {
    pub fn push(&mut self, line: LogLine) {
        if self.lines.len() == LOG_CAP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn all(&self) -> Vec<LogLine> {
        self.lines.iter().cloned().collect()
    }

    /// The newest `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<LogLine> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).cloned().collect()
    }
}

/// `0x08000000 4*16K,1*64K,7*128K`, for diagnostics and the backup sidecar.
pub fn describe_layout(l: &FlashLayout) -> String {
    let segs: Vec<String> = l
        .segments
        .iter()
        .map(|(n, kb)| format!("{n}*{kb}K"))
        .collect();
    format!("{:#010x} {}", l.base, segs.join(","))
}

/// Everything R52 lists, gathered by the caller.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub app_version: String,
    pub os_version: String,
    pub arch: String,
    pub dfu_util_version: String,
    /// The serialized `AppState`.
    pub state: String,
    pub devices: Vec<DfuDeviceInfo>,
    pub layout: Option<String>,
    pub backup_count: usize,
    pub newest_sha256: Option<String>,
    pub log: Vec<LogLine>,
}

/// Renders the report, keeping the last `REPORT_LINES` log lines and
/// replacing `home` with `~` everywhere.
pub fn build(r: &Report, home: Option<&Path>) -> String {
    let mut out = String::new();
    let mut kv = |k: &str, v: &str| out.push_str(&format!("{k}: {v}\n"));
    kv("PocketBoot", &r.app_version);
    kv("macOS", &format!("{} ({})", r.os_version, r.arch));
    kv("dfu-util", &r.dfu_util_version);
    kv("State", &r.state);
    kv("Layout", r.layout.as_deref().unwrap_or("(none)"));
    kv("Backups", &r.backup_count.to_string());
    kv(
        "Newest backup SHA-256",
        r.newest_sha256.as_deref().unwrap_or("(none)"),
    );
    out.push_str("DFU devices:\n");
    if r.devices.is_empty() {
        out.push_str("  (none)\n");
    }
    for d in &r.devices {
        out.push_str(&format!(
            "  {:04x}:{:04x} product={} serial={}\n",
            d.vid,
            d.pid,
            d.product.as_deref().unwrap_or("-"),
            d.serial.as_ref().map_or("-", |s| s.as_str()),
        ));
    }
    out.push_str("Log:\n");
    let skip = r.log.len().saturating_sub(REPORT_LINES);
    for l in r.log.iter().skip(skip) {
        let stream = match l.stream {
            LogStream::Stdout => "stdout",
            LogStream::Stderr => "stderr",
            LogStream::App => "app",
        };
        out.push_str(&format!("  {} [{stream}] {}\n", l.ts, l.text));
    }
    match home.and_then(Path::to_str) {
        Some(h) if !h.is_empty() && h != "/" => out.replace(h, "~"),
        _ => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Serial;

    fn line(i: usize) -> LogLine {
        LogLine {
            ts: "t".into(),
            stream: LogStream::Stdout,
            text: format!("line {i}"),
        }
    }

    #[test]
    fn ring_caps_at_500_dropping_the_oldest() {
        let mut ring = LogRing::default();
        for i in 0..LOG_CAP + 7 {
            ring.push(line(i));
        }
        let all = ring.all();
        assert_eq!(all.len(), LOG_CAP);
        assert_eq!(all[0].text, "line 7");
        assert_eq!(all.last().unwrap().text, format!("line {}", LOG_CAP + 6));
        assert_eq!(ring.tail(2)[0].text, format!("line {}", LOG_CAP + 5));
    }

    #[test]
    fn report_keeps_the_last_200_lines_and_redacts_home() {
        let r = Report {
            app_version: "0.1.0".into(),
            dfu_util_version: "dfu-util 0.11".into(),
            devices: vec![DfuDeviceInfo {
                vid: 0x0483,
                pid: 0xdf11,
                serial: Some(Serial::new("2059")),
                product: Some("STM32 BOOTLOADER".into()),
                location_id: 1,
            }],
            log: (0..250)
                .map(|i| LogLine {
                    text: format!("/Users/pat/Library/x {i}"),
                    ..line(i)
                })
                .collect(),
            ..Default::default()
        };
        let text = build(&r, Some(Path::new("/Users/pat")));
        assert!(!text.contains("/Users/pat"), "{text}");
        assert!(text.contains("~/Library/x 249"));
        assert!(text.contains("~/Library/x 50\n"));
        assert!(!text.contains("~/Library/x 49\n"));
        assert!(text.contains("0483:df11 product=STM32 BOOTLOADER serial=2059"));
        assert!(text.contains("Newest backup SHA-256: (none)"));
    }

    #[test]
    fn layout_reads_as_count_times_size() {
        let l = FlashLayout {
            base: 0x0800_0000,
            segments: vec![(4, 16), (1, 64), (7, 128)],
        };
        assert_eq!(describe_layout(&l), "0x08000000 4*16K,1*64K,7*128K");
    }

    #[test]
    fn log_stream_serializes_lowercase() {
        let j = serde_json::to_value(LogLine::now(LogStream::App, "$ dfu-util -V")).unwrap();
        assert_eq!(j["stream"], "app");
        assert_eq!(j["text"], "$ dfu-util -V");
    }
}
