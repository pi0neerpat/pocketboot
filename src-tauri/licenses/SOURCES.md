# Third-party sources

PocketBoot bundles a `dfu-util` executable, run as a separate process. It is
built from these unmodified source releases, with libusb linked statically:

| Component | Version | License | Source tarball | SHA-256 |
|---|---|---|---|---|
| dfu-util | 0.11 | GPL-2.0-or-later (`dfu-util-COPYING.txt`) | https://dfu-util.sourceforge.net/releases/dfu-util-0.11.tar.gz | `b4b53ba21a82ef7e3d4c47df2952adf5fa494f499b6b0b57c58c5d04ae8ff19e` |
| libusb | 1.0.27 | LGPL-2.1-or-later (`libusb-COPYING.txt`) | https://github.com/libusb/libusb/releases/download/v1.0.27/libusb-1.0.27.tar.bz2 | `ffaa41d741a8a3bee244ac8e54a72ea05bf2879663c098c82fc5757853441575` |

The build steps are in `scripts/build-dfu-util.sh` in the PocketBoot source.

Written offer: for three years from the date you received this copy of
PocketBoot, the PocketBoot maintainers will provide the complete corresponding
source code of the bundled `dfu-util` and `libusb`, for no more than the cost
of physically performing the distribution. The tarballs above are that source.
