---
status: open
kind: tooling
opened: 2026-10-11
---

# Writing a stick from a macOS host

`toyos-flash` writes a stick only from a Linux host and refuses on macOS by
name, so the tree has no way to write a stick from the owner's Mac. A macOS
reader and writer was built and deleted unrun: code that writes disks does not
land untested, and no run with a real stick was possible. It is commit
`094c154df` (`git fetch origin pull/856/head`), `src/flash.rs`'s `registry` and
macOS `host` modules:

- **Reading** — the IOKit registry through `objc2-io-kit` and
  `objc2-core-foundation` 0.3.2, already in `Cargo.lock` through the sysinfo
  fork. Every `IOMedia` with `Whole`; a device's own disk is the one whose
  provider is an `IOBlockStorageDriver`; external and USB are `Protocol
  Characteristics`' `Physical Interconnect Location` (`External`) and
  `Physical Interconnect` (`USB`), searched up the service plane, in the words
  `IOStorageProtocolCharacteristics.h` defines.
- **Identity** — the registry entry ID, and the open descriptor's `st_rdev`
  against the entry's `BSD Major` and `BSD Minor`.
- **Writing** — `getmntinfo` and `unmount(2)` of `/dev/diskN` and every
  `/dev/diskNsM`, behind Disk Arbitration's back; the raw `/dev/rdiskN`; then
  `DKIOCSYNCHRONIZE` with a zeroed `dk_synchronize_t` and `DKIOCEJECT` on the
  writing descriptor (`sys/disk.h`).
- **Unknowns** — whether those three calls succeed on a real stick; an APFS or
  HFS stick's volumes sit on a synthesised disk the unmount does not reach.
- **Fixtures** — this Mac's internal SSD, two APFS-synthesised disks and a
  mounted disk image were recorded from its registry; every USB stick was
  constructed.

It runs as root, so it brings back the row `sudo` on macOS to
`issues/the-build-runs-host-tools-outside-rust-and-qemu.md`, admitted there
because sudo-rs targets only FreeBSD and Linux.

**Exit:** on a Mac with a real USB stick, at the head that lands it, each with
its command, exit code and whole log: the unprivileged run shows the stick as
external USB and prints the `sudo` line, with the registry values it read
committed as a fixture; a root run answered `n` writes nothing and leaves the
stick's volume mounted; a root run answered `y` unmounts, writes, flushes and
ejects; the stick boots the image or reads back equal to it; and the stick
replugged at the prompt is refused.
