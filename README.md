# PocketBoot

A macOS app that starts a battery-less RadioMaster Pocket from USB. It finds the
radio in STM32 DFU mode, saves a validated copy of flash sector 0, and writes the
same bytes back with a DFU leave when you click **Boot radio**.

The product contract and design live in
`plans/2026-10-21-1021-feat-pocketboot-app-spec-plan.md`.

## Prerequisites

- macOS 11 or later, Xcode command-line tools
- Rust (stable), Node 20+, pnpm
- `rustup target add aarch64-apple-darwin x86_64-apple-darwin` for universal builds

## Develop

```sh
pnpm install
scripts/build-dfu-util.sh --stub   # dev stub; only answers -V
pnpm tauri dev
```

The stub lets `tauri-build` resolve `externalBin`, but it cannot talk to a radio.

## Test

```sh
cd src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cd .. && pnpm build
```

### Fixtures are synthetic (Q5 pending)

The dfu-util transcripts in `src-tauri/tests/fixtures/` were written from
dfu-util 0.11 source wording, not captured from a Pocket. See
`src-tauri/tests/fixtures/README.md`. Before release, a human with a radio must:

1. Capture real transcripts for every fixture scenario and replace the files.
2. Re-run `cargo test` and fix any parser pattern that no longer matches.
3. If the captures confirm the vector-table ranges, set
   `VECTOR_CHECK = true` in `src-tauri/src/backup/manager.rs` (KTD9).

## Release build

### 1. Build the real sidecar

```sh
scripts/build-dfu-util.sh
```

This downloads dfu-util 0.11 and libusb 1.0.27, checks them against
`scripts/dfu-util.sha256`, builds both arches with static libusb, and runs
`lipo`. Confirm that `otool -L src-tauri/binaries/dfu-util-universal-apple-darwin`
lists only `/usr/lib` and `/System` libraries.

### 2. Pre-sign the sidecar

```sh
for f in src-tauri/binaries/dfu-util-*-apple-darwin; do
  codesign --force --options runtime --timestamp \
    --sign "Developer ID Application: <Team Name> (<TEAMID>)" "$f"
done
```

### 3. Build, sign, and notarize

```sh
export APPLE_SIGNING_IDENTITY="Developer ID Application: <Team Name> (<TEAMID>)"
export APPLE_API_ISSUER=<issuer-uuid>
export APPLE_API_KEY=<key-id>
export APPLE_API_KEY_PATH=/path/to/AuthKey_<key-id>.p8
pnpm tauri build --target universal-apple-darwin
```

`tauri.conf.json` leaves `signingIdentity` unset, so Tauri reads it from
`APPLE_SIGNING_IDENTITY`. With the `APPLE_API_*` variables set, Tauri notarizes
the app with `notarytool` and staples the ticket. Then staple the DMG:

```sh
xcrun stapler staple src-tauri/target/universal-apple-darwin/release/bundle/dmg/*.dmg
```

### Signing notes

- Hardened runtime, not sandboxed. `Entitlements.plist` is empty on purpose:
  IOKit USB access needs no entitlement outside the sandbox (R44).
- Never add `com.apple.security.cs.disable-library-validation`.

## Licenses

PocketBoot runs dfu-util (GPL-2.0-or-later) as a separate process. dfu-util
links libusb (LGPL-2.1) statically. The license texts and source links are in
`src-tauri/licenses/`, which ships as `PocketBoot.app/Contents/Resources/licenses/`
and opens from the app menu under **Licenses…**.
