// The exact strings from the plan's "Copy per state" (KTD6). `view` maps
// every `AppState` kind to what the screen shows; the `never` check makes a
// new Rust variant a type error here until it has copy.

import type { Action, AppState, DeviceLabel } from "./types";

export type Icon = "radio" | "search" | "save" | "check" | "warning" | "stop";

export type Button = { label: string; action: Action; ariaLabel?: string; disabled?: boolean };

export type ViewModel = {
  icon: Icon;
  headline: string;
  body?: string;
  steps?: string[];
  list?: string[];
  after?: string;
  status?: string;
  spinner?: boolean;
  footnote?: string;
  primary?: Button;
  secondary?: Button;
  assertive?: boolean;
};

const HOLD_STEPS = [
  "Press and hold the power button on the radio.",
  "Keep holding, and click Boot radio.",
];

const BOOT_LABEL = "Boot radio. Hold the radio's power button first.";

function bootButton(disabled = false): Button {
  return { label: "Boot radio", action: "boot", ariaLabel: BOOT_LABEL, disabled };
}

function describe(d: DeviceLabel): string {
  return `${d.product ?? "Unknown device"} (${d.vid}:${d.pid}, serial ${d.serial ?? "none"})`;
}

function formatTime(rfc3339: string): string {
  const t = new Date(rfc3339);
  return Number.isNaN(t.getTime()) ? rfc3339 : t.toLocaleString();
}

function keepHolding(headline: string, percent: number): ViewModel {
  return {
    icon: "radio",
    headline,
    body: "Keep holding the power button. Let go when 4 dots appear on the radio's screen.",
    status: `Restarting the radio… ${percent}%`,
    spinner: true,
    primary: bootButton(true),
  };
}

export function view(s: AppState): ViewModel {
  switch (s.kind) {
    case "starting":
      return { icon: "radio", headline: "Starting…" };
    case "waiting":
      return {
        icon: "search",
        headline: "Connect your radio",
        steps: [
          "Turn the radio off.",
          "Take the batteries out.",
          "Plug the USB cable into the port on the top of the radio.",
        ],
        status: "Looking for the radio…",
        spinner: true,
      };
    case "backingUp":
      return {
        icon: "save",
        headline: "Radio found",
        body: "Saving a safety copy of the radio's startup data. This takes a few seconds.",
        status: `Saving… ${s.percent}%`,
        spinner: true,
      };
    case "ready":
      return s.attempt > 1
        ? {
            icon: "radio",
            headline: "Let's try that again",
            body: "The radio restarted before the power button was held. This time, hold the power button first.",
            steps: [...HOLD_STEPS, "Let go when 4 dots appear."],
            primary: bootButton(),
          }
        : {
            icon: "radio",
            headline: "Ready to boot",
            steps: [...HOLD_STEPS, "Let go when 4 dots appear on the radio's screen."],
            primary: bootButton(),
            footnote: "✓ Safety copy saved.",
          };
    case "booting":
      return keepHolding("Booting — keep holding", s.percent);
    case "restoring":
      return keepHolding("Restoring — keep holding", s.percent);
    case "success":
      return {
        icon: "check",
        headline: "Radio is starting",
        body: "You can let go of the power button. The radio is running on USB power.",
        footnote: "Screen still dark? Unplug the USB cable, plug it back in, and try again.",
      };
    case "failed":
      return {
        icon: "warning",
        headline: "That didn't work",
        body: s.message,
        primary: s.canRetry ? { label: "Try again", action: "retry" } : undefined,
        secondary: { label: "Show log", action: "show_log" },
        assertive: true,
      };
    case "backupFailed":
      return {
        icon: "warning",
        headline: "Couldn't save the safety copy",
        body: s.message,
        after: "Nothing was changed on the radio.",
        primary: { label: "Try again", action: "retry" },
        assertive: true,
      };
    case "conflict":
      return {
        icon: "warning",
        headline: "More than one device is connected",
        body: `PocketBoot found ${s.devices.length} devices in update mode:`,
        list: s.devices.map(describe),
        after: "Unplug the ones you're not using. PocketBoot will continue on its own.",
      };
    case "unsupported":
      return {
        icon: "stop",
        headline: "This doesn't look like a RadioMaster Pocket",
        body: `The connected device (${s.device.product ?? "Unknown device"}, serial ${s.device.serial ?? "none"}) has a different memory layout. PocketBoot only works with the RadioMaster Pocket, so it won't touch this device.`,
      };
    case "dataChanged":
      return {
        icon: "warning",
        headline: "The radio's startup data changed",
        body: `The safety copy from ${formatTime(s.earlierBackupAt)} doesn't match what's on the radio now. This can happen if the cable was unplugged during a boot. PocketBoot won't write anything until you choose.`,
        primary: { label: "Restore earlier copy", action: "restore_earlier_copy" },
        secondary: { label: "Reveal backups in Finder", action: "reveal_backups" },
        // Placeholder copy (Q10).
        footnote: "If you just updated EdgeTX, unplug the radio and quit PocketBoot. The next launch starts fresh.",
      };
    case "fatal":
      return {
        icon: "stop",
        headline: "PocketBoot is damaged",
        body: "A required part of the app is missing or blocked. Download PocketBoot again and replace this copy.",
        secondary: { label: "Copy diagnostics", action: "copy_diagnostics" },
        assertive: true,
      };
    case "driverMissing":
      return { icon: "warning", headline: "A driver is missing", body: describe(s.device) };
    default: {
      const unreachable: never = s;
      return unreachable;
    }
  }
}
