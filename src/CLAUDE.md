# Build system

Loads when you read a file under `src/` — the root cargo project, package name `toyos-build`. Root `CLAUDE.md` has `cargo run` and `cargo test`; the `[profile.toyos]` rule is this crate's to enforce (`src/build.rs`).

## Boot modes

- `cargo run -- --gop` boots a UEFI GOP display (`-vga std`) instead of virtio-gpu: the config where the on-screen panic console renders.
- `cargo run -- --diag-boot --build-only` builds `target/bootable-diag.img`, the diagnostic boot for a machine with no serial port: its config declares no `devices`, so the kernel's log stays readable off the panel. Same kernel and bootloader binaries as the ordinary build. **A flashable artifact is built from a committed tree** — `cargo` builds the working tree.
- `cargo run -- --console-boot --build-only` builds `target/bootable-console.img` — `/system/bin/console`, the shell on the raw framebuffer, for asking a machine questions instead of reflashing it.
- `cargo run -- --kernel-param <name>` (repeatable, any boot mode) arms an actuator at boot; every actuator lives in one kernel built with `boot-actuators` (`kernel/src/actuator.rs`), so it decides no kernel build. `--kernel-feature <name>` *is* a build; it accepts any feature `kernel/Cargo.toml` declares, and that set is closed by `cargo test --lib`. An undeclared name is refused by name before any lock.
- `cargo run -- --metal-sim` boots the T14's hardware shape (GOP + NVMe + xHCI + i8042, no virtio, no USB HID, with a 16550); `--mute` takes the serial away, the T14's literal shape.

## Other entry points

- **`.github/qemu-version` is the QEMU every guest is measured with, declared once**; every guest job's first step (`src/ci.rs`'s `instrument`) reds on a disagreement.
- `system.toml` defines which programs to build and the init sequence.
- **A workflow step runs `cargo run -- --ci <job>` and nothing else; logic in YAML is a defect.**

## The toolchain store

- **Every LLVM, compiler and sysroot is a directory of the store** (`src/store.rs`): `rust/build/<kind>/<key>/` in the primary, one per key, read-only, placed whole by a rename or not at all, and made by whichever checkout first needs it — the primary is no different. A key is one function of a recipe and the git hashes of the four trees the toolchain is built from: the rust fork, `toyos-abi`, `toyos` and `userland/libc`, edits included. A build compiles against its own sysroot's key, so two worktrees with different ABIs never refuse each other.
- **The rustup `toyos` toolchain names `rust/build/toyos`**, a link the primary's build moves to its sysroot by a rename.
- **A toolchain is built in a fork checkout beside the building worktree's ABI trees** (`src/sysroot.rs`): the primary's `rust/`; a linked worktree's own `rust/` while it holds fork work its pin does not (`git -C <primary>/rust worktree add --detach <worktree>/rust <pin>` makes one); otherwise the host's one shared checkout, `<primary>/rust/build/fork/`, which such builds make toolchains in one at a time. A worktree whose `rust/` is the stub holds no fork state.
- **A killed build places nothing**; the bootstrap the kill orphans runs on in its fork checkout's build directory.
- A lock is `flock` on a directory (`src/dirlock.rs`), and every blocking one repeats itself every 30 s — a queue is never silence.

## Worktrees

- `git worktree add --no-track -b wt/<name> <path> origin/main` makes one and `git worktree remove <path>` takes it. A worktree holding a fork checkout of its own in `rust/` is refused by git until that checkout goes first. Never `git submodule update` in a linked worktree: that is a second 913 MiB clone of the fork.
- Everything under a worktree — targets, images, its fork checkout — is its own; the object stores, the store and the rustup link are the primary checkout's, and ownership is derived from `git rev-parse --git-common-dir`, never recorded.
- **A linked worktree's `main` ref is only as current as the primary's last `--sync`: anything asking "does this branch differ from main" diffs against `origin/main`.**
- **Type-checking a std edit without building a sysroot**: point `__CARGO_TESTS_ONLY_SRC_ROOT` at a tree holding an APFS clone of `rust/library` (`cp -Rc`), a workspace `Cargo.toml` naming `library/std`, and symlinks to `toyos-abi`/`toyos`; then `CARGO_TARGET_DIR=<scratch> cargo +toyos build -Z build-std=std,panic_abort --target x86_64-unknown-toyos --offline`. Delete `<scratch>/**/.fingerprint/std-*` between runs — cargo does not re-fingerprint std under `-Zbuild-std`.

## Caveats that bite every agent

- **Every CI lane is GitHub-hosted and no workflow may name a self-hosted label** — a `runs-on:` naming one queues until it times out rather than failing, so `src/ci.rs`'s `workflows_run_against_main_on_hosted_runners` refuses it; a measurement owed on hardware goes to the metal loop, not to a runner.
- **A workflow job that runs in a container adds `safe.directory` itself** — `actions/checkout` sets it into a temporary global config it discards when its step ends, so the first git command a container step runs after checkout dies on a dubiously-owned repository.
