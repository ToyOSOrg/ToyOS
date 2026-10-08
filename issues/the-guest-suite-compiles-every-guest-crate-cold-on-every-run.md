---
status: open
kind: tooling
opened: 2026-10-08
---

# The guest suite compiles every guest crate cold on every run

`guest.yml`'s `suite` restores the sysroot and nothing compiled with it: every
kernel, loader, userland crate and test binary a guest boots is built in the
job, for both architectures. Three jobs of 2026-10-08, in seconds, by each
log's own timestamps and the suite's `building` and `testing` figures: A is
113110642665, a merge group; B is 113190683753, a pull request that moved the
kernel; C is 113188345621, one that moved a single userland crate.

| | A | B | C |
|---|---|---|---|
| `deps`: apt and rustup | 103 | 171 | 141 |
| the driver's build | 146 | 102 | 148 |
| the harness's build | 54 | 36 | 53 |
| the suite, building | 610 | 368 | 542 |
| the suite, testing | 136 | 124 | 131 |
| the job | 1072 | 821 | 1036 |

The `BUILT` lines behind `building`, A / B / C:

- x86_64 kernel, loader, ROOT of tests/netcase: 138 / 84 / 123
- x86_64 test kernel, ROOT of tests/testcases: 65 / 37 / 56
- x86_64 ROOT of tests/metalcase: 212 / 121 / 186
- aarch64 test kernel, loader, ROOT of tests/testcases: 110 / 70 / 97
- aarch64 kernel: 33 / 20 / 31
- aarch64 `mask-windows` kernel, ROOT of tests/virtsmpcase: 33 / 21 / 30
- the test binaries and the small ROOTs together: 19 / 15 / 19

Two of those costs are gone since. The metalcase line is: the one guest test
that booted it boots tests/panelcase, whose `BUILT` line read 8 s on the
development machine where metalcase's read 53 s after the same builds. And
the harness's build no longer compiles ring, rustls, ureq and the build system
a second time. No runner has measured either.

What an entry of main's targets could save a run is bounded by its `building`
figure. With every target fresh the whole suite's builds took 23 s on the
development machine, 8 s of them an AArch64 userland that was not. A run that
moved the kernel compiles it five times whatever is restored, once per feature
set and architecture: the two lines above that are a kernel and little else
are 20 to 33 s each.

What an entry would hold, measured on the development machine after one whole
suite: the four target directories are 724,457,182 B in 2953 files without
cargo's incremental state and 2,259,625,434 B of incremental state beside it;
`tar | zstd -T0` of the first is 201,025,465 B.

What stands in the way:

- The repository's caches are past GitHub's 10 GB and no gate reads a stored
  size (`issues/the-host-caches-limit-reaches-the-10-gb-only-through-one-measured-ratio.md`).
- An entry names target directories, and
  `issues/the-tree-resolves-in-five-cargo-locks-not-one.md` folds `kernel/target`,
  `bootloader/target` and `userland/target` into the root's.
- Only main's scope is read by every ref, so the writer is a run on main, and
  no pull request can show the saving before the writer has landed.
- A restored target is trusted by content or not at all: `src/cicache.rs` is
  the tree's one reader that does.

Not taken: `CARGO_INCREMENTAL=0` in the job. A cold
x86-64 kernel took 43 s of CPU and left 35 MiB without it, 62 s and 339 MiB with
it, on the development machine under load. It also turns rustc's MIR inliner
on (`kernel/Cargo.toml`, `[profile.toyos]`), so the job would boot other bytes
than `cargo run` builds.

Owner: the guest job (`src/ci.rs`'s `guest`, `.github/workflows/guest.yml`).

**Exit**: `guest / suite` on a pull request that moves no guest source prints
a `building` figure under 60 s.
