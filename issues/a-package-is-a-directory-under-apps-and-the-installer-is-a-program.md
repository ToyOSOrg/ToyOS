---
status: open
kind: track
opened: 2026-09-04
---

# A package is a directory under /apps, and the installer is a program

## Approved package behavior

- Keep the base small. Ship package GUI and CLI with one catalogue/state/authority
  model; download browsers, development tools, editor, SSH and additional drivers.
  Bundle drivers needed to boot, install offline, use the desktop and reach packages.
  Recommend applicable driver and CPU microcode packages; microcode payloads are
  distributed separately from the OS image.
- Ordinary app-specific libraries travel with the app. No custom install, update
  or uninstall scripts. Apps initialize/migrate private data on launch, including
  selecting separately supplied game data. Driver/OS activation needs its own design.
- Support the distro catalogue and external package files; additional catalogues
  are deferred. Unsigned packages require explicit approval and an unverified
  publisher indication. No silent enrollment of an external update source.
- Personal installs require the user's approval; all-user installs and system
  changes require system authority. An installer is an ordinary authorized
  program, not a privileged executable name. Protect committed installed versions
  from arbitrary writes, using private staging and authorized activation.
- Package identity comes from protected installation records and verified update
  transitions. A name or digest alone grants nothing. Verified updates preserve
  approvals and denials; unverified replacement cannot inherit them.
- Prepare verified app updates automatically, activate while stopped and allow
  per-app pause. No automatic private-data recovery points or built-in backup product.
- Quit before uninstalling; offer keep/delete for private data, preserve unrelated
  documents and forget saved permissions. Updates are not uninstall/reinstall.
- Allow unpackaged unsigned native programs. Direct approvals last only that run;
  ordinary inherited/delegated access follows its existing lifetime.
- Apps manage plugins with general isolation tools; no OS extension framework.
  Separately installed drivers require explicit device authority and supported
  isolation; being signed does not confer the distro's isolation exception.
- Native source ports and libc are the compatibility path; Linux binary execution
  is deferred. Stabilize the app contract within a defined support window when
  ready; current development remains free to change interfaces.

The final package metadata, protected identity and activation protocol remain to
design. Ordinary runtime permission requests require no manifest category list.
Network consent and delegation are owned by the permission-runtime track; do not
implement a separate package-specific policy.

## Milestone

Install and launch gbae as an ordinary package with authorized access to selected
game data. Use the same package behavior for DOOM and other native ports.

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
  licence the archive carried. Today's installer adds a `manifest.toml` naming
  the package, version, archive digest and program to launch. The launcher can
  discover packages by listing `/apps`; this does not exclude protected
  installation/source identity records. Package metadata is not authority.
  The final metadata format remains open.
- **A package is a program, whole** (owner, 2026-10-02): self-contained and
  fixed at build time. No library packages, no dependency solver, no install
  scripts; a fix in what a package carries is a rebuild of the package.
- Not his ruling, a refinement put to him with it that he did not object to: a
  library can be a build input named exactly by hash, never an installed
  thing; shared objects stay, only library packages go.
- **`/apps/<name>` is immutable by stage-then-commit** (owner, 2026-10-02).
  An authorized installer unpacks into private staging and activates the
  verified version in one step. Follow the installed-code protection policy
  above; the activation interface remains to be designed.
- **A standalone C program's ToyOS changes are patch files in a recipe**
  (owner, 2026-10-02), against an upstream archive pinned by hash; a delta too
  large to read as patches moves to a fork repository pinned by commit. Forks
  stay for crates, `rust/` and LLVM. Recipes and patches live in this
  repository, where `NOTICE` and the licence gate carry them, not in a ports
  repository.
- **The installer is an ordinary program**, `/system/bin/pkg`, with explicit
  installation authority as approved above. It fetches, verifies, stages and
  activates packages. Manual installation keeps its confirmation, separate from
  runtime resource consent. `pkg install <name>`, `pkg install <file>`, `pkg remove
  <name>`, `pkg list`.
- **Verification is by a signed repository** (owner ruling: `pkg install
  <url>` and the release's `SHA256SUMS` behind it are retired): a root pinned
  in the image, then a timestamp and a targets naming each archive by length
  and SHA-256 (`lib/update/src/repo.rs`, written by `build/publish.rs`).
  `pkg install <file>` stays for local and offline installs. Today's adjacent
  `SHA256SUMS` check establishes consistency with that file, not an authenticated
  source identity; it cannot confer an existing signed app's saved permissions.
- **Fetching is HTTPS.** GitHub serves releases only over TLS, so `pkg`
  carries a TLS client; the network stack under it is netd's. A crate that
  does TLS is not our job to write and is widely used; it takes the fork
  treatment every third-party crate takes. The redirect GitHub answers with
  is followed once.
- **Running is the desktop's.** gbae opens a window through winit and
  softbuffer and plays through cpal, all three on the forks the SDK release
  branches carry. It lists a directory itself and reads the ROM the user picks
  out of it. The first run is the milestone's end. Under stage 5's view that
  listing reaches no ROM outside its own folder
  (`issues/an-installed-gbae-browses-to-no-rom.md`).

## Stages, in order

The storage track's users and mount-protocol stages do not block this one.

1. Landed: `pkg install <file>`, `pkg remove`, `pkg list`. Its judge,
   `pkg_install_gbae`, is a metal row
   `issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`
   owes. **What a package holds is the image's `[apps]` row, never
   its own `manifest.toml`** — that file names which of the directory's
   binaries a launch starts and nothing else, so a writable `/apps` cannot name
   a device or a right. The stage-then-commit above amends it: `pkg` writes
   `/apps/<name>/` in place today, its `manifest.toml` last
   (`system/pkg/src/main.rs`).
2. The HTTPS fetch: TLS client under `pkg`, the GitHub redirect, the signed
   repository's files and the archives its targets names. This is the
   internet-client track's last
   stage (`issues/the-internet-clients-work-unchanged.md`). Judged in QEMU against a server the harness
   runs on the host in Rust; then once against GitHub itself, by hand, with the owner
   watching. No registered test fetches anything.
3. Updates: `pkg install` of a newer version replaces the directory whole
   after verification and through explicit installation authority. Implement
   the installed-code protection and finished-distro activation policies above.
   **Exit**: direct writes cannot change a committed version, failed verification
   cannot activate one, and activation cannot mix old and new files in a running
   application. The old version remains available until replacement succeeds.
4. The signed repository. Landed: the verifier and the publisher, on the
   host. Owed: `pkg install <name>` from a mirror list whose one kind is a
   local directory, the pinned root and its floors under `/system/etc/pkg/`,
   the machine's under `/state/pkg`, and the commit by rename.
5. The users track's per-user `/home`
   (`issues/a-user-is-a-home-tree-and-a-login-row.md`) decides
   where a package's own data goes. Until then it goes in its own folder of
   the session user's home, `/home/toy/Apps/<name>`, which is its `HOME` and
   the one part of the home it sees; its own `/apps/<name>` is read-only to
   it (`toyos_manifest::Program::view`).
6. **New access needs authorization within the image's ceiling** (amended
   owner ruling, 2026-10-16). Follow the runtime consent and delegation rules
   in `issues/permissions-and-capabilities-need-one-runtime-contract.md`;
   package declarations are no longer a prerequisite for a permission request.
   Ordinary access such as a window, sound playback and the app's own data
   directory remains part of the default app environment. **The ceiling names
   rights, never apps**: it is the set any installed app may be granted at all,
   so installing an app never edits the image and the image never lists an
   installed app by name. The `[apps]` row of stage 1 becomes that ceiling.
   **Exit**: an installed app obtains additional authority only through an
   authorized grant or transfer; requests beyond the ceiling are refused by
   name, and changing package metadata grants nothing.
7. **The apps leave this repository.** Each app (snake first, as the pilot:
   it builds unchanged for every OS) moves to its own repository, built with
   only the published SDK crates and the released toolchain, and published as
   a release archive, the shape gbae already has, which the signed
   repository's targets names. The image then carries none of them; `pkg
   install <name>` brings them. Blocked by stage 6.
   **Doom goes at this stage too** (owner ruling, 2026-09-26): the `doom`
   crate with the doomgeneric C it compiles, `assets/DOOM1.WAD`, and
   `assets/soundfont.sf2`, which doom alone opens, leave the image as one
   installable package. Until they do, `build/licence.rs` names each as an
   exception pending this stage.

## Open with the owner

- The libraries the machine provides: GPU, video.
- The future application compatibility boundary and support window.
- Scale.
