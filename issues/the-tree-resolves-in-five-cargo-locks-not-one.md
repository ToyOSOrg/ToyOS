---
status: assigned
kind: track
opened: 2026-10-04
---

# The tree resolves in five Cargo locks, not one

The root, `kernel/`, `bootloader/`, `userland/` and `toyos/` are five
Cargo resolutions with five locks, and four of them declare their own
`[profile.toyos]`. A
crate two of them share is tested on the host against the root's lock and
shipped from another's, and the locks disagree on versions: once the
userland lock is aligned, 8 registry pairs sit below the highest version
another of the five locks carries in the same semver range, all of them the
loader's.

**The owner ruled on 2026-10-04.** Asked "Merge everything into one Cargo
workspace (after small version-alignment steps; proven by byte-identical
kernel and loader)?", he chose "Yes, one workspace (Recommended)": "One
lockfile, one profile, shared crates tested as shipped; about 35 version
alignments land first."

**Owner:** the orchestrator. Everything below is the orchestrator's plan and
its measurements, not the ruling.

**Exit:** one workspace and one lock at the root replace the five. The step
that merges them moves no version: its lock's name and version pairs are the
union of the aligned locks, and the kernel and the loader, both arches, are
byte-identical to the build of the last alignment before it. The root
`.cargo/config.toml` is tracked, which `.gitignore` ignores today and where
`.claude/agents/implementer.md` has agents list fork clones, so the merge
moves that instruction; no `rust-toolchain.toml` is tracked outside `rust/`,
since step 1 of the rule in `issues/the-tree-says-who-uses-each-thing.md`
names none. The alignments
land first, each in today's workspace: pull request #724 aligned the root
and kernel locks, the userland lock is the second, and the loader's 8 pairs
are the last.

**Constraints, measured** on a scratch workspace seeded from the five locks
at `8b4f88446`, by type-check and one loader link, with no boot:

- One lock holds one version per semver range, so every pair below its
  range's maximum moves up to it. At `8b4f88446` that was 35: the 29 below
  the maximum then (userland 18, loader 7, root 3, kernel 1), the 3 that
  userland's moves pull with them (`ureq-proto`, `utf-8`, `zeroize_derive`),
  and the root's registry `getrandom` 0.2, 0.3 and 0.4, which become the forks
  userland patches in at the same versions. Those three move no version: they
  follow from the root `[patch]` and land with it. What the loader's `libc`,
  added since, pulls with it is not measured.
- `userland/Cargo.lock` carries `miniz_oxide` 0.8.9, for `png` 0.18.1,
  beside 0.9.1, for `flate2` 1.1.10. Built with only `flate2` back at 1.1.9,
  compositor and files are 204 to 228 bytes of text smaller on x86_64 and
  704 on aarch64, and in neither build does a symbol of a third
  `miniz_oxide` ship in them: `flate2`'s and std's are the two they carry.
  Exit: `png` takes `miniz_oxide` 0.9.
- Every fork commit the five locks pin stays pinned, and the crypto
  pre-releases (`ed25519-dalek 3.0.0-pre.6`, `pkcs5 0.8.0-rc.13`) are only in
  `userland/Cargo.lock` and stay.
- The loader's `[profile.toyos]` has no `strip = "debuginfo"`; a loader built
  with and without it differs in 247,720 of 328,192 bytes. One profile is a
  loader byte change, made before the merge.
- `[patch]` is workspace-wide. `tests/toyos-rust-tests` patches memmap2,
  `tests/toyos-rust-tests/tls-cranelift`, a resolution with its own lock,
  patches target-lexicon, and `tests/ssh-client-host` takes upstream tokio,
  so all three stay outside the workspace.
- The guest triples' flags stay per triple: with no
  `[target.x86_64-unknown-uefi]` table, `curve25519-dalek-derive` enters the
  x86_64 loader's graph.
