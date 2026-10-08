---
status: open
kind: tooling
opened: 2026-10-08
---

# The guest suite compiles every guest crate cold on every run

`guest.yml`'s `suite` restores the sysroot and nothing compiled with it: every
kernel, loader, userland crate and test binary a guest boots is built in the
job, for both architectures. Four jobs of 2026-10-08, in seconds, by each
log's own timestamps and the suite's `building` and `testing` figures: A is
113110642665, a merge group; B is 113190683753, a pull request that moved the
kernel; C is 113188345621, one that moved a single userland crate; D is
113260475768, the merge group that put the one workspace on main (#746,
`1084ddc9a`), and the only one of the four that builds as the tree now does.

| | A | B | C | D |
|---|---|---|---|---|
| `deps`: apt and rustup | 103 | 171 | 141 | 116 |
| the driver's build | 146 | 102 | 148 | 125 |
| the harness's build | 54 | 36 | 53 | 45 |
| the suite, building | 610 | 368 | 542 | 465 |
| the suite, testing | 136 | 124 | 131 | 133 |
| the job | 1072 | 821 | 1036 | 911 |

The `BUILT` lines behind `building`, A / B / C / D:

- x86_64 kernel, loader, ROOT of tests/netcase: 138 / 84 / 123 / 89
- x86_64 test kernel, ROOT of tests/testcases: 65 / 37 / 56 / 54
- x86_64 ROOT of tests/metalcase: 212 / 121 / 186 / 158
- aarch64 test kernel, loader, ROOT of tests/testcases: 110 / 70 / 97 / 91
- aarch64 kernel: 33 / 20 / 31 / 27
- aarch64 `mask-windows` kernel, ROOT of tests/virtsmpcase: 33 / 21 / 30 / 27
- the test binaries and the small ROOTs together: 19 / 15 / 19 / 19

Two of those costs were taken out after D, and are in none of the four. The
metalcase line: the one guest test that booted it boots tests/panelcase, whose
`BUILT` line read 8 s on the development machine where metalcase's read 53 s
after the same builds. And the harness's build: in all four logs it compiles
ring, rustls, rustls-webpki, ureq and the build system a second time, and
`ci::dispatch` no longer hands a step the variables that made its cargo do so.

What an entry of main's targets could save a run is bounded by its `building`
figure. A run that moved the kernel compiles it five times whatever is
restored, once per feature set and architecture: the two lines above that are
a kernel and little else are 20 to 33 s each.

Measured on the development machine before the workspace fold, when the guests
built into four target directories that are now `target/<triple>/toyos/`, and
not measured since:

- With every target fresh the whole suite's builds took 23 s, 8 s of them an
  AArch64 userland that was not.
- After one whole suite the four directories were 724,457,182 B in 2953 files
  without cargo's incremental state and 2,259,625,434 B of incremental state
  beside it; `tar | zstd -T0` of the first was 201,025,465 B.

What stands in the way:

- The repository's caches are past GitHub's 10 GB and no gate reads a stored
  size (`issues/the-host-caches-limit-reaches-the-10-gb-only-through-one-measured-ratio.md`).
- Only main's scope is read by every ref, so the writer is a run on main, and
  no pull request can show the saving before the writer has landed.
- A restored target is trusted by content or not at all: `src/cicache.rs` is
  the tree's one reader that does.

Not taken: `CARGO_INCREMENTAL=0` in the job. A cold
x86-64 kernel took 43 s of CPU and left 35 MiB without it, 62 s and 339 MiB with
it, on the development machine under load. It also turns rustc's MIR inliner
on (the root `Cargo.toml`, `[profile.toyos]`), so the job would boot other bytes
than `cargo run` builds.

Owner: the guest job (`src/ci.rs`'s `guest`, `.github/workflows/guest.yml`).

**Exit**: `guest / suite` on a pull request that moves no guest source prints
a `building` figure under 60 s.
