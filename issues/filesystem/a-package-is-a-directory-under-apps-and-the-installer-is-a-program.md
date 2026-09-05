---
status: open
kind: track
opened: 2026-09-04
---

# A package is a directory under /apps, and the installer is a program

The milestone the owner set on 2026-09-03: download and install
github.com/Japabu/gbae from its GitHub release and run it, in QEMU first,
with no shortcut past what an ordinary OS target does. What this track
rules is below; the five decisions that were the owner's were answered on
2026-09-05 and are written in.

## What exists on the other side

gbae v0.2.0 publishes `gbae-v0.2.0-toyos-x86_64.tar.gz` (604,872 bytes:
`gbae/gbae`, an `x86_64-unknown-toyos` PIE built with the published SDK
crates and the released toolchain; `gbae/LICENSE`; `gbae/README.md`) beside
a `SHA256SUMS` covering every archive of the release, at
`https://github.com/Japabu/gbae/releases/download/v<version>/<file>`. Its
CI builds and links for ToyOS and will not boot it (owner ruling): running
the binary in a ToyOS guest is this track's harness's job, not gbae's.

## The shape

- **A package is the release's built archive.** Binary only, for now:
  fetching a source tree and building it on the machine waits for a hosted
  compiler and is no part of this track.
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
  before it installs — at install, the moment the user typed the command,
  never at first run, so a refusal leaves nothing on disk and no answer has
  to be stored. `pkg install <url>`, `pkg install <file>`, `pkg remove
  <name>`, `pkg list`.
- **Verification is by the release's own `SHA256SUMS` first**:
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
  branches carry. It reads a ROM the user chose, from `/home/<user>`, through
  the file picker. The first run is the milestone's end.

## Stages, in order

Stage 1 began when the hierarchy landed as #401; the storage track's users
and mount-protocol stages follow this track rather than block it.

1. `pkg install <file>` from a local archive on the boot stick, with the
   digest checked against a `SHA256SUMS` beside it: the layout, the
   manifest, the launcher row, the consent prompt, removal. Judged in QEMU
   by the harness placing the archive on the image and reading `/apps`
   back off the DATA volume after the guest is gone. The archive is the
   real gbae release asset, so this stage already runs gbae once.
2. The HTTPS fetch: TLS client under `pkg`, the GitHub redirect, the sums
   file from the same release. Judged in QEMU against a server the harness
   runs on the host in Rust, then once against GitHub itself, by hand, with
   the owner watching.
3. Updates: `pkg install` of a newer version replaces the directory whole
   after the new archive verified; the old one is gone only after the new
   one is in place.
4. Signatures over the sums file, from a key the owner publishes with the
   project.
5. The users track's per-user `/home`
   (`issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`) decides
   where a package's own data goes; until then a package writes under
   `/apps/<name>/` only.
