#!/usr/bin/env bash
# Builds the universal dfu-util sidecar (R23, R41, R42).
#
#   scripts/build-dfu-util.sh          dfu-util 0.11 + static libusb 1.0.27
#
# Output: src-tauri/binaries/dfu-util-{universal,aarch64,x86_64}-apple-darwin
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/src-tauri/binaries"
TRIPLES=(universal-apple-darwin aarch64-apple-darwin x86_64-apple-darwin)

DFU_URL="https://dfu-util.sourceforge.net/releases/dfu-util-0.11.tar.gz"
USB_URL="https://github.com/libusb/libusb/releases/download/v1.0.27/libusb-1.0.27.tar.bz2"

install_all() {
  local src="$1"
  mkdir -p "$OUT"
  for t in "${TRIPLES[@]}"; do
    cp "$src" "$OUT/dfu-util-$t"
    chmod 0755 "$OUT/dfu-util-$t"
  done
}

export MACOSX_DEPLOYMENT_TARGET=11.0
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

curl -fsSL -o dfu-util-0.11.tar.gz "$DFU_URL"
curl -fsSL -o libusb-1.0.27.tar.bz2 "$USB_URL"
shasum -a 256 -c "$ROOT/scripts/dfu-util.sha256"

for arch in arm64 x86_64; do
  host="$([[ $arch == arm64 ]] && echo aarch64-apple-darwin || echo x86_64-apple-darwin)"
  prefix="$WORK/prefix-$arch"
  cflags="-arch $arch -mmacosx-version-min=11.0"

  rm -rf libusb-1.0.27 && tar -xjf libusb-1.0.27.tar.bz2
  (cd libusb-1.0.27 &&
    ./configure --host="$host" --prefix="$prefix" --disable-shared --enable-static CFLAGS="$cflags" &&
    make -j"$(sysctl -n hw.ncpu)" && make install)

  rm -rf dfu-util-0.11 && tar -xzf dfu-util-0.11.tar.gz
  (cd dfu-util-0.11 &&
    ./configure --host="$host" CFLAGS="$cflags" \
      USB_CFLAGS="-I$prefix/include/libusb-1.0" \
      USB_LIBS="$prefix/lib/libusb-1.0.a -framework IOKit -framework CoreFoundation -framework Security" &&
    make -j"$(sysctl -n hw.ncpu)")
  cp dfu-util-0.11/src/dfu-util "$WORK/dfu-util-$arch"
done

lipo -create -output "$WORK/dfu-util" "$WORK/dfu-util-arm64" "$WORK/dfu-util-x86_64"

# Only system libraries may be linked dynamically. Check each thin binary
# separately so lipo's architecture headings are not mistaken for dylibs.
for arch in arm64 x86_64; do
  if otool -L "$WORK/dfu-util-$arch" | tail -n +2 | grep -vE '^\s+(/usr/lib/|/System/)'; then
    echo "error: dfu-util ($arch) links a non-system dylib" >&2
    echo "inspect with: otool -L '$WORK/dfu-util-$arch'" >&2
    exit 1
  fi
done

install_all "$WORK/dfu-util"
"$OUT/dfu-util-universal-apple-darwin" -V | head -1
echo "sidecar written to $OUT"
