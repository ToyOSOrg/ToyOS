---
status: assigned
kind: track
opened: 2026-10-04
---

# The tree resolves in five Cargo locks, not one

The root, `kernel/`, `bootloader/`, `userland/` and `toyos/` were five
Cargo resolutions with five locks, and three of them declared their own
`[profile.toyos]`. A crate two of them shared was tested on the host against
the root's lock and shipped from another's. The pull request that carries
this text folds them into one workspace and one lock.

**The owner ruled on 2026-10-04.** Asked "Merge everything into one Cargo
workspace (after small version-alignment steps; proven by byte-identical
kernel and loader)?", he chose "Yes, one workspace (Recommended)": "One
lockfile, one profile, shared crates tested as shipped; about 35 version
alignments land first."

**Owner:** the orchestrator. Everything below is the orchestrator's plan and
its measurements, not the ruling.

**Exit:** one workspace and one lock at the root replace the five. The step
that merges them moves no version: its lock's name and version pairs are the
union of the aligned locks. It changes nothing a kernel or a loader is built
from but the workspace root, shown for both architectures by two
measurements at one absolute path:

- the kernel and the loader are byte-identical to a control that is the base
  with only the workspace root moved up, keeping the crate's own base lock,
  its profile and its flags;
- every `rustc` command line `cargo build -v` runs for them is the base's,
  once the tree's path and cargo's path-derived hashes are taken out.

The root `.cargo/config.toml` is tracked, so a fork clone under edit is
listed in the untracked `.cargo/local.toml` it includes; no
`rust-toolchain.toml` is tracked outside `rust/`, since step 1 of the rule in
`issues/the-tree-says-who-uses-each-thing.md` names none. The alignments
landed first, each in its own workspace: pull request #724 aligned the root
and kernel locks, #732 the userland lock, and #738 the loader's lock and its
profile's `strip`.

**The exit's first wording was the orchestrator's and could not be met.** It
asked for a kernel and a loader byte-identical to the base's own build. Cargo
hashes each path package's path, relative to the workspace root, into `-C
metadata`, and hands rustc a path package's source by that relative path, so
moving the root changes every path crate's symbol names and every source path
compiled into the artifact: a panic or a lock site the kernel named
`src/hardlockup/probe.rs` it now names `kernel/src/hardlockup/probe.rs`, and a
path dependency outside the old root, named by its absolute path, is named
from the repository root. The base did not
reproduce its own bytes either. Measured at `257ebea2a`: the base built at a
second path differed from itself in all four artifacts, its x86_64 kernel
carrying 38 strings that name the checkout and its AArch64 kernel 35
(`issues/two-checkouts-of-one-tree-build-different-guest-bytes.md`). The
orchestrator ruled the wording above in its place on 2026-10-07.

**What is left:** the T14's run of the metal profile on the folded build,
which that pull request stages.

**What stays apart, and why:**

- `[patch]` is workspace-wide. `tests/toyos-rust-tests` patches memmap2,
  `tests/toyos-rust-tests/tls-cranelift`, a resolution with its own lock,
  patches target-lexicon, and `tests/ssh-client-host` takes upstream tokio,
  so all three stay outside the workspace.
- `userland/libc` keeps its own lock because that lock is an input of the
  sysroot key (`src/sysroot.rs`, `SYSROOT_MANIFESTS`): as a member it would be
  resolved by the root lock, and every dependency change of any member would
  move the key and rebuild every sysroot. Its `panic = "abort"` profile is not
  the reason; a named root profile would carry that. The price is a sixth
  resolution: its lock resolves `toyos`, `toyos-abi`, `toyos-elf`,
  `toyos-osrelease` and `dlmalloc` again, and nothing holds it to the root's.
  Both carry `dlmalloc` 0.2.13 today.
- The guest triples' flags stay per triple: with no
  `[target.x86_64-unknown-uefi]` table, `curve25519-dalek-derive` enters the
  x86_64 loader's graph.

**Constraints the fold kept:**

- One lock holds one version per semver range. The root's registry
  `getrandom` 0.2, 0.3 and 0.4 became the forks userland patched in at the
  same versions: they moved no version and followed from the root `[patch]`.
- `Cargo.lock` carries `miniz_oxide` 0.8.9, for `png` 0.18.1, beside 0.9.1,
  for `flate2` 1.1.10. Exit of that pair: `png` takes `miniz_oxide` 0.9.
- Every fork commit the five locks pinned stays pinned, and the crypto
  pre-releases (`ed25519-dalek 3.0.0-pre.6`, `pkcs5 0.8.0-rc.13`) stay.
