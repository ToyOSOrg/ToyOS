---
status: open
kind: track
opened: 2026-09-04
---

# A package is a directory under /apps, and the installer is a program

The milestone the owner set on 2026-09-03: download and install
github.com/Japabu/gbae from its GitHub release and run it, in QEMU first,
with no shortcut past what an ordinary OS target does. What this track
rules is below; the five decisions still the owner's are in
`issues/filesystem/five-package-decisions-are-the-owners.md`, each with the
recommendation this track assumes until answered.

## What exists on the other side

gbae v0.2.0 publishes `gbae-v0.2.0-toyos-x86_64.tar.gz` (604,872 bytes:
`gbae/gbae`, an `x86_64-unknown-toyos` PIE built with the published SDK
crates and the released toolchain; `gbae/LICENSE`; `gbae/README.md`) beside
a `SHA256SUMS` covering every archive of the release, at
`https://github.com/Japabu/gbae/releases/download/v<version>/<file>`. Its
CI builds and links for ToyOS; nothing runs it there yet, and that is this
track's job.

## The shape

- **A package is a directory.** `/apps/<name>/` holds the binaries, data and
  licence the archive carried, unpacked as-is, plus one file the installer
  writes: `manifest.toml` naming the package, version, the archive's digest,
  and the program to launch. Nothing is registered anywhere else: the
  desktop's launcher lists `/apps` and reads each manifest; init grants an
  app its namespace from that row the same way `system.toml` grants a system
  program's. Removal is deleting the directory.
- **The installer is an ordinary program**, `/system/bin/pkg`, with no
  authority a shell does not have: it fetches, verifies, unpacks and writes
  under `/apps` because `/apps` is writable to it, and it asks the user
  before it installs (decision 3). `pkg install <url>`, `pkg install
  <file>`, `pkg remove <name>`, `pkg list`.
- **Verification is by the release's own `SHA256SUMS` first** (decision 2):
  the installer fetches the sums file from the same release, checks the
  archive against it, and refuses on mismatch or absence. Signatures are a
  later stage of the same file, not a different mechanism.
- **Fetching is HTTPS.** GitHub serves releases only over TLS, so `pkg`
  carries a TLS client; the network stack under it is netd's. A crate that
  does TLS is not our job to write and is widely used; it takes the fork
  treatment every third-party crate takes. The redirect GitHub answers with
  is followed once.
- **Running is the desktop's.** gbae opens a window through winit and
  softbuffer and plays through cpal, all three on the forks the SDK release
  branches carry. It lists a directory itself and reads the ROM the user picks
  out of it. The first run is the milestone's end.

## Stages, in order

1. Landed. `pkg install <file>`, `pkg remove`, `pkg list`, judged by
   `pkg_install_gbae`: gbae v0.2.0's own release archive verified against its
   `SHA256SUMS`, unpacked into `/apps/gbae`, launched, and 1,621,313 bytes read
   back off the DATA volume with the guest gone. **What a package holds is the
   image's `[apps]` row, never its own `manifest.toml`** — that file names which
   of the directory's binaries a launch starts and nothing else, so a writable
   `/apps` cannot name a device or a right. gbae needs no `HOME`: its keyboard
   layout is `toyos/src/surface.rs`'s fixed `LAYOUT_CONFIG`, whose directory the
   kernel creates at boot, and it opens no file picker.
2. The HTTPS fetch: TLS client under `pkg`, the GitHub redirect, the sums
   file from the same release. Judged in QEMU against a server the harness
   runs on the host in Rust, then once against GitHub itself, by hand, with
   the owner watching.
3. Updates: `pkg install` of a newer version replaces the directory whole
   after the new archive verified; the old one is gone only after the new
   one is in place.
4. Signatures over the sums file, from a key the owner publishes with the
   project.
5. The users track's per-user `/home` decides where a package's own data
   goes; until then a package writes under `/apps/<name>/` only.

## Not decided here

The five in the owner's file: binary-first or source-first, the trust
chain's first form, consent at install versus at first run, whether gbae's
CI should run its binary on ToyOS, and the sequencing against the storage
track's remaining stages.
