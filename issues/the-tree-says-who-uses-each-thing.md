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
loader/   the UEFI loader (today bootloader/)
kernel/   CLAUDE.md; src/ pure/ loom/ sim/ pci/ gicv3/ ps2/ dma/ cpuvuln/ microcode/ (+ intel-ucode, licence)
system/   CLAUDE.md (the server doctrine); one directory per /system/bin name:
          supervisor compositor (desktop/ sprite/ wallpaper.jpg)
          netstack (mdns/ dns/ and the net family) soundserver (mixer/) console diskserver
          fileserver logkeeper filepicker pkg update swap sshserver shell terminal toybox inspect host
apps/     CLAUDE.md (sdk/ and registries only; Linux under Wayland, macOS, Windows)
          calc editor files paint snake doom/ (doomgeneric/ DOOM1.WAD soundfont licences);
          toyfetch when built
sdk/      CLAUDE.md (identity, sysroot, publication; std links abi and toyos)
          abi/ toyos/ keymap/ font/ window/ filepicker/ libc/ (arch/)
lib/      acpi bcachefs blackbox blockhold blockring bootmap elf elide fat32 gpt hda i219 inspect
          logstream manifest osrelease quiesce rootimage swap symbols tco tmpdir tsc untrusted
          update userbound wallclock xhci
share/    mirrors /system/share: fonts/ (+ OFL) icons/ (+ MIT)
images/   system.toml console.toml diag.toml
build/    CLAUDE.md; the root package's library and binaries (today src/); ci/qemu-version
tests/    CLAUDE.md; toyos.rs checks.rs common/ checks/ (the root package's test targets)
          guest/ (proctest kernelprobe metalprobe test-runner rust-tests/) images/ (one .toml per boot case)
          machines/t14/ (package toyos-metal: the loop, the descriptor, the Linux timings)
          judges/ssh/ corpus/tinycc/
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
- `lib/osrelease` is `toyos-osrelease`, which #722 added after the design;
  the build, `libc` and the supervisor use it (rule step 5).
- `lib/tsc` is `toyos-tsc`, which #721 added after the design; the kernel
  and the loader use it (rule step 5).

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
2. **Version alignment** in today's workspaces:
   `issues/the-tree-resolves-in-five-cargo-locks-not-one.md`.
3. **One workspace:** `issues/the-tree-resolves-in-five-cargo-locks-not-one.md`.
   **Exit:** that file's.
4. **std off repository paths:** `issues/std-names-the-sdk-crates-by-path.md`.
   **Exit:** that file's.
5. **`sdk/`:** abi, toyos, keymap, font, window, filepicker and libc (with
   `tests/libc-arch` as `sdk/libc/arch/`); `src/sysroot.rs`,
   `src/sdkversion.rs` and `src/release.rs` read the directory instead of a
   list. A branch that bumps the `rust` gitlink merges the fork change with
   it. **Exit:** `PUBLISHED` and `SYSROOT_SOURCES` are gone or derived from
   `sdk/`; a release dry run, a clean toolchain build and the guest suite
   green.
6. **`lib/`, and what one thing uses into that thing:** `kernel/cpuvuln`,
   `kernel/microcode`, the net family and `toyos-dns` into
   `system/netstack/`. **Exit:** no `toyos-*` or `bcachefs` directory is left
   at the root; `git diff -M` reports every change a
   rename; `--ci host` and the guest suite green.
7. **`userland/` splits into `system/`, `apps/` and `tests/guest/`;**
   programs resolve by name; `userland/CLAUDE.md` becomes `system/CLAUDE.md`;
   `files` drops `sprite` and `sprite` moves into the compositor. **Exit:**
   `userland/` is gone; no `apps/` package depends on a path outside `sdk/`;
   `--build-only` and the guest suite green.
8. **Tests and images:** `images/`, `tests/images/`, `tests/machines/t14/`
   (`src/metal*.rs`, `tests/metal/` and `tests/t14-linux/` as package
   `toyos-metal`), `tests/judges/ssh/`, `tests/corpus/tinycc/`, and
   `.github/qemu-version` as `build/ci/qemu-version`. Image names stay
   `bootable-<stem>.img`. Blocked on
   `issues/the-owners-flash-script-runs-diskutil.md`, which owns
   `diag/flash.sh`. **Exit:** no `tests/*case/`, `console/` or `diag/`
   directory and no root `system.toml` remains, and `--diag-boot`,
   `--console-boot` and a `--metal-readback` build green.
9. **`src/` becomes `build/`, `bootloader/` becomes `loader/`,** and
   `examples/imgstat.rs` becomes a binary of the root package in `build/`
   (rule step 2). **Exit:** `src/`, `bootloader/` and `examples/` are gone;
   `--ci host` and `--build-only` green.
10. **`assets/` into `share/`, `apps/doom/` and `system/compositor/`**;
    each licence text beside what it covers; `OPENED_BY` goes; `doom.jpg`
    and `first-boot.jpg`, which only the README shows, into `.github/`
    (rule step 1). Needs
    `issues/five-apps-read-the-system-font-by-a-path-only-this-tree-has.md`
    first, since `calc` and `snake` read `../../assets` at build time.
    **Exit:** `assets/`, `licenses/` and both photos are gone from the root;
    the image's file list is identical before and after; the licence gate is
    green.
