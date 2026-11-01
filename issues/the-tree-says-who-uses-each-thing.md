---
status: assigned
kind: track
opened: 2026-10-04
---

# The tree says who uses each thing

The repository's top level is shaped by cargo's per-directory `build.target`,
not by what each thing is or who uses it. The owner adopted a layout on
2026-10-04; this file records it and the migration to it.

**Owner:** the orchestrator. The rule, the rulings and the tree are the
owner's, the tree changed only where a later ruling or `main` moved it (listed
under it); the stages and their exits are the orchestrator's plan.

## The rule

> **A path says who uses a thing. A thing lives inside the one thing that uses
> it. A thing that several things use lives under the top level named for what
> they share. A top level exists for a boundary, or because a tool outside this
> repository looks there, and the rule names every one of those.**

**Uses** is the only relation the rule reads, and it is defined once:

- **X uses Y** when X links Y, or when X's code or build script reads Y's bytes
  by name.
- **A shipped file is used by whoever reads its installed form.**
- **Assembling is not a use.** Assembling is the build reading a thing to
  compile, copy, link, convert or sign it into an image or a toolchain.
- **Starting a program is not a use, and neither is running a tool.**

None of these facts depends on which machine builds the tree, ToyOS included.

**Decision procedure.** Apply the steps in order; the first that fits wins.

1. **Does a tool outside this repository look for it at a fixed place?** Then it
   goes there, and nothing else does. The complete list:
   - **cargo:** `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `clippy.toml`.
   - **git:** `.gitignore`, `.gitmodules`.
   - **the forge, which renders the root:** `README.md`, `LICENSE-*`, `NOTICE`.
   - **GitHub:** `.github/`, which holds the workflows, the PR template, and the
     pictures only the README shows.
   - **Claude Code:** `.claude/`, plus each `CLAUDE.md`, which sits at the top of
     the subtree whose caveats it holds.

   A file our own code reads is never here, whatever its subject. That is why
   `qemu-version` leaves `.github/`.
2. **Does nothing use it?** Then it is a root, placed by what it is:
   - **`kernel/`:** what the loader boots.
   - **`loader/`:** what firmware boots.
   - **`system/<name>/`:** a program the image is; one directory per
     `/system/bin` name.
   - **`apps/<name>/`:** a program built on `sdk/` and registries alone, which
     the image does not need in order to be itself. It leaves this repository at
     stage 7.
   - **`images/`:** an image definition.
   - **`build/`:** the build system, meaning the root package's library and
     binaries.
   - **`tests/`:** anything that exists to judge. That covers the root package's
     test targets, guest test programs, test images, machine descriptors and the
     loops that drive them, second implementations, and corpora. One exception:
     a test that judges a single thing with nothing else present is part of that
     thing (step 4). **Shipping in an image does not change this.**
   - **`ports/<name>/`:** a third-party program packaged by a recipe, with its
     archive pinned by hash and its patch files (owner ruling, 2026-10-02).
   - **`rust/`:** the toolchain the tree is built with, a submodule. LLVM lives
     inside it.
   - **`issues/`:** the record, flat.

   A crate fork is not a path at all. It is a ToyOSOrg branch, named once in the
   root `[patch]`.
3. **Does something outside this repository use it?** Then `sdk/`. That covers
   crates.io's crates, libc as the C face, and what std links.
4. **Does exactly one thing use it?** Then it lives inside that thing. This
   covers a crate, data, a firmware blob, vendored upstream source compiled into
   that thing, a licence text, a host test, and a `CLAUDE.md`. The root package
   counts as one thing: what its library uses lives in `build/`, and what its
   test targets use lives in `tests/`.
5. **Do several things use it?** Then:
   - code goes to `lib/`, and is held to the bar of its strictest user;
   - shipped data goes to `share/`, which mirrors `/system/share`.
6. **Does nothing use it yet?** This is staged code. It goes where its track
   names its first user.

"Stage 7" above is stage 7 of
`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`.

## The owner's rulings, 2026-10-04

Each is the option he chose, then its text, verbatim.

- **Rule and top level:** "Take it as proposed (Recommended)": "The rule and
  the 16-folder tree above; src/ becomes build/, bootloader/ becomes loader/,
  userland/ splits into system/, apps/ and tests/guest/."
- **One workspace:** "Yes, one workspace (Recommended)": "One lockfile, one
  profile, shared crates tested as shipped; about 35 version alignments land
  first."
- **Tracker:** "Flatten it (Recommended)": "Cheapest step, done first; 574
  citations in 284 files updated."
- **Apps:** "calc, doom, editor, files, paint, snake (Recommended)": "The six
  user apps; toyfetch joins them when built. Everything else is system/."
- **filepicker:** "Publish it (Recommended)": "It becomes a public SDK crate
  that apps may use; editor and paint keep using it."
- **sprite:** "Files drops sprite (Recommended)": "One fewer public crate to
  commit to; files draws what it needs itself."
- **App fonts:** "SDK call for the font (Recommended)": "An SDK call answers
  the platform's monospace font: /system/share/fonts on ToyOS, the platform's
  own on Linux, macOS, Windows. Also fixes the three apps that can't start on
  a host."
- **toyos-ld** has no place in the tree: the owner ruled it deleted ("Just get
  rid of toyos ld"), and pull request #726 deletes it.

## The tree

```
README.md CLAUDE.md NOTICE LICENSE-APACHE LICENSE-MIT            rule step 1
Cargo.toml Cargo.lock clippy.toml .cargo/config.toml             rule step 1: one workspace, one lock,
                                                                 one [target.<triple>] table per guest triple
.github/ .claude/ .gitignore .gitmodules                         rule step 1
loader/   the UEFI loader, package bootloader
kernel/   CLAUDE.md; src/ pure/ loom/ sim/ pci/ gicv3/ ps2/ dma/ microcode/ (+ intel-ucode, licence)
system/   CLAUDE.md (the server doctrine); one directory per /system/bin name:
          supervisor compositor (desktop/ sprite/)
          netstack (mdns/ dns/ and the net family) soundserver (mixer/) console diskserver
          fileserver logkeeper filepicker pkg update swap sshserver shell terminal toybox inspect host
apps/     CLAUDE.md (sdk/ and registries only; Linux under Wayland, macOS, Windows)
          calc editor files paint snake doom/ (doomgeneric/ DOOM1.WAD soundfont licences);
          toyfetch when built
sdk/      CLAUDE.md (identity, sysroot, publication; std links abi and toyos)
          abi/ toyos/ std/ keymap/ font/ window/ filepicker/ osrelease/ libc/ (arch/)
lib/      acpi bcachefs blackbox blockhold blockring bootmap elf elide fat32 gpt hda inspect
          logstream manifest quiesce rootimage swap symbols tco tmpdir tsc untrusted
          update userbound wallclock xhci
share/    mirrors /system/share: fonts/ (+ OFL) icons/ (+ MIT)
images/   system.toml console.toml diag.toml
build/    CLAUDE.md; the root package's library and binaries (bin/ toyos-flash imgstat); ci/qemu-version;
          ssh/ (package toyos-ssh)
tests/    CLAUDE.md; toyos.rs checks.rs common/ checks/ (the root package's test targets)
          guest/ (proctest kernelprobe metalprobe test-runner rust-tests/) images/ (one .toml per boot case)
          metal/ (package toyos-metal: the loop)
          corpus/tinycc/ cavp/
ports/    <name>/recipe.toml and patches/, empty until the first
issues/   <slug>.md
rust/     the submodule
```

A directory drops `toyos-` and its package keeps it (`lib/gpt` is package
`toyos-gpt`). `system.toml` resolves a program key in `system/`, `apps/` and
`tests/guest/` and refuses a name found in two.

Where this differs from the tree the owner adopted:

- `system/toyos-ld` is gone, by his toyos-ld ruling.
- `filepicker-api` is `sdk/filepicker/`, by the filepicker ruling.
- `sprite` is inside the compositor rather than in `lib/` or `sdk/`: by the sprite
  ruling `files` stops using it, which leaves the compositor its one user
  (rule step 4).
- `apps/` names toyfetch, by the Apps ruling.
- `sdk/osrelease` is `toyos-osrelease`, which #722 added after the design;
  the sysinfo fork, outside this repository, uses it from crates.io (rule
  step 3).
- `lib/tsc` is `toyos-tsc`, which #721 added after the design; the kernel
  and the loader use it (rule step 5).
- `sdk/std/` is std's ToyOS backend, which left the `rust` fork after the
  design (the owner: "MOVE IT"); the fork's `library/std`, outside this
  repository, compiles it by `#[path]` (rule step 3).
- `toyos-i219` is `system/i219/driver`: the harness stopped using it in
  93719bc03, after the design, which leaves the `i219` driver program its
  one user (rule step 4).
- `toyos-pci-claim`, `acpiserver-api` and `logkeeper-api` are in `lib/`:
  each has two users (rule step 5).
- `toyos-ssh` is `lib/ssh`: `build/signing.rs` and `system/sshserver` use it
  (rule step 5).
- `tests/metal/` is the metal loop. A machine's records are in the host's
  store since #852, so there is no `tests/machines/t14/` and no descriptor.
- `tests/cavp/` stays: `toyos-crypto`'s `tests/nist.rs` reads it. It was
  ruled shared test data, which inside a crate's directory the licence gate
  would count as shipping.
- There is no `tests/judges/ssh/`: no SSH judge is in the tree.

## Stages

Each stage updates, in its own diff, every prompt and `CLAUDE.md` pointer that
names a path it moves.

**Exit of the track:** `git ls-tree --name-only HEAD` prints exactly the
tree's top level: `.cargo`, `.claude`, `.github`, `.gitignore`,
`.gitmodules`, `CLAUDE.md`, `Cargo.lock`, `Cargo.toml`, `LICENSE-APACHE`,
`LICENSE-MIT`, `NOTICE`, `README.md`, `apps`, `build`, `clippy.toml`,
`images`, `issues`, `kernel`, `lib`, `loader`, `rust`, `sdk`, `share`,
`system`, `tests`, and `ports` once it holds a recipe.

0. **Hygiene, no moves.** Landed with the commit that filed this track:
   every package manifest has a `description`, and
   `.cargo/config.toml.example`, `kernel/README.md` and
   `kernel/TECHNICAL_DEBT.md` are gone. **Exit:** every tracked
   `[package]` outside `rust/` has a `description` line.
1. **A flat tracker.** Landed in #723.
2. **Version alignment.** Landed in #724, #732 and #738.
3. **One workspace.** Landed in #746.
4. **std off repository paths.** Landed with stages 5-9, in one pull
   request. **Exit:** std names both by version; the build supplies them by
   one `[patch]`; a clean toolchain build passes; the userland targets'
   `libstd`/`libtoyos` have identical `.rlib` members and byte-identical
   `.rmeta` and `.so` before and after, each object member's name aside,
   which carries rustc's per-invocation token; the keys move once, in the
   commit that names both by version, through the fork's `library/`,
   `Cargo.toml` and `Cargo.lock` and the keyed std text. The closed
   `std-names-the-sdk-crates-by-path.md`'s exit, "the sysroot key is
   unchanged by the switch", is not met: every key moved.
5. **`sdk/`:** abi, toyos, keymap, font, window, filepicker, osrelease and libc (with
   `tests/libc-arch` as `sdk/libc/arch/`). The moves landed with stage 4, in
   one pull request, with a clean toolchain build and the guest suite green.
   **Remaining:** `build/sysroot.rs`'s `SYSROOT_SOURCES` and
   `build/sdkversion.rs`'s `PUBLISHED` are still lists. `SYSROOT_SOURCES`
   cannot come from `sdk/` alone: libc links `toyos-elf`, which is in
   `lib/elf` (rule step 5). `PUBLISHED` leaves out `sdk/libc`,
   `sdk/libc/arch` and `sdk/std`, which are not published, and
   `sdk/filepicker`, which the filepicker ruling makes public. No release
   dry run was made: `release` and `publish` run only on a push to `main`.
   **Exit:** `PUBLISHED` is derived from `sdk/` and names `sdk/filepicker`;
   `SYSROOT_SOURCES` is derived from `sdk/` and the `lib/` crates libc links;
   a release dry run green.
6. **`lib/`, and what one thing uses into that thing:** `kernel/cpuvuln` (deleted by #915),
   `kernel/microcode`, the net family and `toyos-dns` into
   `system/netstack/`. Landed with stage 4, in one pull request. **Exit:** no
   `toyos-*` or `bcachefs` directory is left at the root; `git diff -M`
   reports no file added or deleted by the move; `--ci host` and the guest
   suite green.
7. **`userland/` splits into `system/`, `apps/` and `tests/guest/`;**
   programs resolve by name; `userland/CLAUDE.md` becomes `system/CLAUDE.md`;
   `files` drops `sprite` and `sprite` moves into the compositor. Landed with
   stage 4, in one pull request. **Exit:** `userland/` is gone; no `apps/`
   package depends on a path outside `sdk/`; `--build-only` and the guest
   suite green.
8. **Tests and images:** `images/`, `tests/images/`, `tests/metal/` (the
   metal loop as package `toyos-metal`; a machine's records are in the
   host's store, so there is no descriptor), `tests/guest/rust-tests/`, `tests/guest/c/` and
   `tests/corpus/tinycc/`. Image names stay `bootable-<stem>.img`.
   `whole_disk` and `Node`, which `toyos-flash` and the loop both read, are
   in `build/disk.rs`. Landed with stage 4, in one pull request. **Exit:** no
   `tests/*case/`, `console/` or `diag/` directory and no root `system.toml`
   remains, and `--diag-boot`, `--console-boot` and a `--metal-readback`
   build green.
9. **`src/` becomes `build/`, `bootloader/` becomes `loader/`,**
   `examples/imgstat.rs` becomes a binary of the root package in `build/`
   (rule step 2), `.github/qemu-version` becomes `build/ci/qemu-version`, and
   `toyos-ssh` becomes `build/ssh`. Landed with stage 4, in one pull request.
   **Exit:** `src/`, `bootloader/` and `examples/` are gone; `--ci host` and
   `--build-only` green.
10. **`assets/` into `share/`, `apps/doom/` and `system/compositor/`**;
    each licence text beside what it covers; `OPENED_BY` goes; `doom.jpg`
    and `first-boot.jpg`, which only the README shows, into `.github/`
    (rule step 1). Needs
    `issues/five-apps-read-the-system-font-by-a-path-only-this-tree-has.md`
    first, since `calc` and `snake` read `../../assets` at build time.
    **Exit:** `assets/`, `licenses/` and both photos are gone from the root;
    the image's file list is identical before and after; the licence gate is
    green.
