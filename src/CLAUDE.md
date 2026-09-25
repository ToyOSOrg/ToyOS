# Build system

Loads when you read a file under `src/` — the root cargo project, package name `toyos-build`. Root `CLAUDE.md` has `cargo run` and `cargo test`; the `[profile.toyos]` rule is this crate's to enforce (`src/build.rs`).

## Boot modes

- `cargo run -- --gop` boots a UEFI GOP display (`-vga std`) instead of virtio-gpu: the config where the on-screen panic console renders.
- `cargo run -- --diag-boot --build-only` builds `target/bootable-diag.img`, the diagnostic boot for a machine with no serial port: its config declares no `devices`, so the kernel's log stays readable off the panel. Same kernel and bootloader binaries as the ordinary build. **A flashable artifact is built from a committed tree** — `cargo` builds the working tree.
- `cargo run -- --console-boot --build-only` builds `target/bootable-console.img` — `/system/bin/console`, the shell on the raw framebuffer, for asking a machine questions instead of reflashing it.
- `cargo run -- --kernel-param <name>` (repeatable, any boot mode) arms an actuator at boot; every actuator lives in one kernel built with `boot-actuators` (`kernel/src/actuator.rs`), so it decides no kernel build. `--kernel-feature <name>` *is* a build; it accepts any feature `kernel/Cargo.toml` declares, and that set is closed by `cargo test --lib`. An undeclared name is refused by name before any lock.
- `cargo run -- --metal-sim` boots the T14's hardware shape (GOP + NVMe + xHCI + i8042, no virtio, no USB HID, with a 16550); `--mute` takes the serial away, the T14's literal shape.

## Other entry points

- `cargo run -- --check-forks` names every lockfile pin behind the fork branch its manifest consumes. **On demand only: it asks the network**, so it is in neither `cargo test` nor the landing gate.
- **`.github/qemu-version` is the QEMU every guest is measured with, declared once**; every guest job's first step (`src/ci.rs`'s `instrument`) reds on a disagreement. `cargo test --lib` refuses a workflow that installs QEMU without naming its instrument (`src/sourcegate.rs`).
- `system.toml` defines which programs to build and the init sequence.
- **A workflow step runs `cargo run -- --ci <job>` and nothing else; logic in YAML is a defect.**

## The host's locks and slots

- **Sysroots are content-addressed** (`src/sysroot.rs`): one per key — the identity (`src/identity.rs`, so a comment is no change) of `toyos-abi/src`, `toyos/src`, `userland/libc/src` and their manifests, the std fork's `library/` and `src/bootstrap/`, and the compiler — at `rust/build/sysroots/<key>/`, made by whichever worktree first needs it and never written again. Every build compiles against its own key's, so two worktrees with different ABIs never refuse or wait for each other; the only shared step is the primary's compiler, which a sysroot build reads under the global lock in shared mode. A new key costs one std build of the three guest targets; `--worktree remove` sweeps the keys no worktree records.
- **The std fork is built per worktree, and nothing but the primary's own sync moves the primary's `rust/`.** A linked worktree's `rust/` becomes, on its first build, a git worktree of the primary's fork repository at the commit its tree pins — that is where the fork is edited, committed and pinned. A fork commit whose `compiler/` is not the one the primary's compiler was built from is refused by name: a compiler change lands, and the primary's sync and next build rebuild it. If that checkout later falls behind the commit its tree pins (a merge moved the pin), the build moves the checkout to it itself, fetching from the primary's repository first if it holds the commit, unless the checkout has local changes, which it refuses to move out from under.
- `src/buildlock.rs` serialises the stateful phases in two scopes: `Global` (the primary's compiler and the rustup link — one directory in `.git/`, shared by every worktree) and `Worktree` (the crate-target cleans, and this worktree's std build). Only `./x.py` typed by hand in `rust/` escapes it.
- **Never kill a build that has taken the global lock** — the kill removes the shell wrappers, not the bootstrap, which inherits the file descriptor and runs on regardless; a toolchain rebuild interrupted or unobserved this way can leave `stage2/bin` without a `cargo`.
- **The host hands out guest slots and build slots** — `buildlock::guest_slot` (twelve across every worktree, one per task) and `buildlock::build_slot` (four), separate counts so a suite holding every guest slot can still compile. **The order is a constraint at every acquirer**: host slot → a sysroot key's lock → build lock → artifact. Every blocking lock names its holders and repeats itself every 30 s — a queue is never silence. `cargo test --test toyos-build -- --host-slots N --host-builds N` overrides, 0 turns either off.

## Worktrees

- Everything under a worktree — targets, images, `.build-locks/`, its fork checkout — is its own; the object stores, the compiler and the rustup link are the primary checkout's, and ownership is derived from `git rev-parse --git-common-dir`, never recorded.
- **A linked worktree's `main` ref is only as current as the primary's last `--sync`: anything asking "does this branch differ from main" diffs against `origin/main`.**
- **Type-checking a std edit without building a sysroot**: point `__CARGO_TESTS_ONLY_SRC_ROOT` at a tree holding an APFS clone of `rust/library` (`cp -Rc`), a workspace `Cargo.toml` naming `library/std`, and symlinks to `toyos-abi`/`toyos`; then `CARGO_TARGET_DIR=<scratch> cargo +toyos build -Z build-std=std,panic_abort --target x86_64-unknown-toyos --offline`. Delete `<scratch>/**/.fingerprint/std-*` between runs — cargo does not re-fingerprint std under `-Zbuild-std`.

## Caveats that bite every agent

- **Documentation carries no gates** — `src/redlist.rs` resolves doc paths only because it gates a Rust table, not a corpus.
- **Every CI lane is GitHub-hosted and no workflow may name a self-hosted label** — a `runs-on:` naming one queues until it times out rather than failing, so `src/ci.rs`'s `workflows_run_against_main_on_hosted_runners` refuses it; a measurement owed on hardware goes to the metal loop, not to a runner.
- **A workflow job that runs in a container adds `safe.directory` itself** — `actions/checkout` sets it into a temporary global config it discards when its step ends, so the first git command a container step runs after checkout dies on a dubiously-owned repository.
- **A red build may be the build system — re-run in isolation before believing any single red.** A `stage1-std/<target>/dist/deps` temp-dir error means a concurrent build, never a broken checkout; never repair or force-rebuild the toolchain.
