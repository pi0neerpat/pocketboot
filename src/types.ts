// Mirrors the Rust `View` and `LogLine` serialization, field for field.

export type DeviceLabel = { vid: string; pid: string; serial?: string; product?: string };

export type ReasonCode =
  | "deviceGone"
  | "accessDenied"
  | "busy"
  | "notWriteable"
  | "usbIo"
  | "timeout"
  | "validationSize"
  | "validationBlank"
  | "hashMismatch"
  | "sidecarMissing"
  | "unknown";

export type AppState =
  | { kind: "starting" }
  | { kind: "waiting" }
  | { kind: "conflict"; devices: DeviceLabel[] }
  | { kind: "unsupported"; device: DeviceLabel }
  | { kind: "backingUp"; device: DeviceLabel; percent: number }
  | { kind: "ready"; device: DeviceLabel; attempt: number; backupAt: string }
  | { kind: "booting"; device: DeviceLabel; attempt: number; percent: number }
  | { kind: "restoring"; device: DeviceLabel; percent: number; backupAt: string }
  | { kind: "success"; device: DeviceLabel }
  | { kind: "failed"; device?: DeviceLabel; reason: ReasonCode; message: string; canRetry: boolean }
  | { kind: "backupFailed"; device?: DeviceLabel; reason: ReasonCode; message: string }
  | { kind: "dataChanged"; device: DeviceLabel; earlierBackupAt: string }
  | { kind: "fatal"; reason: ReasonCode; message: string }
  // Reserved for Windows; never emitted on macOS.
  | { kind: "driverMissing"; device: DeviceLabel };

export type LogLine = { ts: string; stream: "stdout" | "stderr" | "app"; text: string };

/// Commands the frontend may invoke. None takes a path, address, or data (R15).
export type Action = "boot" | "retry" | "restore_earlier_copy" | "reveal_backups" | "copy_diagnostics" | "show_log";
