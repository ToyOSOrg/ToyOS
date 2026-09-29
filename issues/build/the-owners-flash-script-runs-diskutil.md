---
status: open
kind: tooling
opened: 2026-09-29
---

# The owner's flash script runs `diskutil`, and no ledger declares it

`diag/flash.sh` writes an image to a USB stick through `diskutil` and `plutil`,
which are macOS binaries, and through `shasum`, `dd` and `sudo`. The README's
flashing steps also run `diskutil`. The metal loop does not use this script: it flashes
the T14's stick over `ssh` from Ubuntu.

The script's two gates are what keep it off an internal drive: it takes only
disks that `diskutil list external physical` names, and it writes only to a
disk that reports `Internal=false` and `BusProtocol=USB` on its own account. A
replacement keeps both.

**Exit**: `rg -l "diskutil|plutil"` over the tree outside `rust/` finds nothing, and
the build system writes the stick.
