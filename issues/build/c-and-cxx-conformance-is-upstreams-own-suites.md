---
status: open
kind: track
opened: 2026-09-29
---

# C and C++ conformance is upstream's own suites, judged by their own oracles

`tests/testcases/tinycc/` is replaced by three permissively licensed upstream
suites plus our own cases. Each test is built by the toolchain's clang, run as a
ToyOS process, and judged by its suite's own oracle. Every number here comes from
a command that was run, except where it says *estimate*.

## The suites

| suite | pin | carried | tests |
|---|---|---|---|
| musl libc-test, MIT | `repo.or.cz/libc-test.git` `7b95dfa5f5d5` (it has no tags) | 815 files, 1,378,866 bytes | 74 functional, 68 regression, 133 math, 79 api headers (compile-only) |
| llvm-test-suite `SingleSource`, Apache-2.0 WITH LLVM-exception plus each carried directory's own `LICENSE` | `llvmorg-22.1.8` = `28d2f36a29e8`, the release of the LLVM the fork builds | 894 of 4,008 files, 10,175,372 bytes | 372 with a `.reference_output`: 249 C and 115 C++ portable, 6 x86-64-only, 2 AArch64-only |
| libc++ `libcxx/test`, Apache-2.0 WITH LLVM-exception | the `ToyOSOrg/llvm-project` commit `rust/` pins (`a79bc52c`), whose `libcxx/test` tree `eb1b3645` equals base `52ed14fc`'s | nothing new | 7,751 `.pass`, 732 `.compile.pass`, 552 `.verify`, 193 `.compile.fail`, 42 `.sh`, 12 `.gen.py` |

**What is left out, and why:**

- libc-test: `src/math/crlibm/` (GPL-2.0 vectors); `src/math/ucb/` (Sun's terms
  forbid redistribution for a fee, the same class as the DECUS notice NOTICE
  refuses); the 66 math tests that include either; four `_dso` companions and
  `musl/pleval`, which are not tests.
- SingleSource: `gcc-c-torture` (GPL); `Polybench` (its licence makes the user
  indemnify OSU); `Benchmarks/Misc` (per-file terms, one of them PSF);
  ObjC, Mips, Altivec, HVX, LoongArch, AVX-512 and AMX (no ToyOS target or
  CI guest has them); `SetjmpLongjmp` (upstream's own CMake skips it).

## Decisions

**Each suite is carried in-tree, unmodified, and locked to upstream's git object
ids.** A fetch at build time is out, because the owner ruled that tests may not
fetch (NOTICE, gbae). A `ToyOSOrg` mirror repository is out too: it adds a
checkout that every worktree and every CI job must make, to carry 11.5 MB that
this repository can hold directly, and it proves nothing the lock does not.

- **Layout.** `tests/conformance/<suite>/upstream/` holds upstream's paths
  byte for byte. The pin is a `tier = "suite"` row in `forks.toml`, beside
  doomgeneric's.
- **The lock.** `<suite>/suite.toml` records upstream's object id for every
  carried path: a tree id for a whole directory, a blob id for a single file.
  An importer writes it from the pinned commit. Nobody writes it by hand.
- **The gates.** A host test reds when `git ls-tree HEAD` under `upstream/`
  disagrees with a lock row, or finds a path no row covers.
  `cargo run -- --check-forks` re-reads upstream at the pin and reds on any row
  upstream does not have.
- **Nothing under `upstream/` is edited.** Flags live in `suite.toml`,
  including the knobs SingleSource's CMake sets. The 40 carried
  `CMakeLists.txt` set `FP_TOLERANCE` 6 times, `FP_ABSTOLERANCE`,
  `RUN_OPTIONS` and `HASH_PROGRAM_OUTPUT` 3 times each, and name a flag
  variable 37 times. A gate reds when a carried `CMakeLists.txt` sets one of
  these knobs and `suite.toml` does not.
- **Updates.** An update is one PR: the importer runs at the new pin, and the
  lock and the red rows move with it.
- **libc++ is the exception.** Its tests are read from the same llvm-project
  checkout the libc++ under test is built from, so the tests cannot describe
  another version. Its gate is `<pin>:libcxx/test` equal to `<base>:libcxx/test`.
  The toolchain release carries `std`, `libcxx`, `extensions` and `support`
  (39,724,239 bytes) beside libc++ for a job that has no checkout.
- **NOTICE.** Each carried suite gets one NOTICE section: its SPDX line, pin
  and exclusions.

**Folder structure:**

```
tests/conformance/
  Cargo.toml, src/   toyos-conformance: manifests, lit, oracles, the guest judge
  ours/              <case>.c and <case>.expect, MIT OR Apache-2.0
  libc-test/         suite.toml, upstream/
  llvm-test-suite/   suite.toml, upstream/SingleSource/
  libcxx/            suite.toml
```

A test is registered as `<suite>::<upstream path without extension>`.
**An upstream test is judged only by upstream's oracle**, and we never write an
expectation for one:

- libc-test: the exit status;
- SingleSource: the `.reference_output` with its `exit N` line, under fpcmp's
  tolerances;
- libc++: the file kind, and clang `-verify` for `.verify` tests.

**`src/redlist.rs` stays the one known-red mechanism**: one row shape, one
`check` (a registered name and an `expected-red` issue), and one
`--known-red`.

- **Where the rows live.** A suite's red rows are stored in its `suite.toml`
  and loaded by `src/redlist.rs`.
- **The stage.** Each row names the stage its test stops at: `compile`,
  `link` or `run`. It is attempted to that stage on every run, and it reds
  when it gets further. That is `NOT_RUN`'s rule, and these rows replace
  `NOT_RUN`. A row with no stage is not run, the same as a machine test's row.
- **`unsupported` is not a red.** It is a test of something ToyOS declines by
  design. It cites the rule and carries no issue.
- **The pass count.** The count is registered minus rows minus unsupported. It
  is printed per suite on every run and never stored. A failing test with no
  row reds. So does a passing row, until someone deletes it. The history of the
  rows is the record over time.

**The runner is `toyos-conformance`, one std crate** that builds for the host
and for the ToyOS target.

- **The library** enumerates a suite, evaluates its directives, emits each
  test's clang command, and judges an outcome.
- **The binary** is the guest judge. It runs each test as a child with stdout
  and stderr on pipes, judges it, and prints one verdict line.
- **Today**, `tests/toyos.rs` compiles on the host, packs the stripped
  binaries into shared boots by image bytes, and reads only the verdict lines.
  A libc-test binary averages 575,217 bytes stripped and 1.72 MB unstripped,
  over 34.
- **After M2**, the same binary builds and runs a suite inside ToyOS.
- **What it replaces:** `ccheck`, `check_c_result`, `NOT_RUN`, `C_METAL_SKIP`
  and `the_two_comparisons_use_one_rule`. It is one judge compiled twice, so no
  second copy has to be held equal.

**lit, without Python.** Over the pinned tree, libc++ uses:

- `UNSUPPORTED` (6,447 lines), `XFAIL` (1,094) and `REQUIRES` (1,051). 527 of
  these lines use `&&`, `||` or `!`, and 33 feature tokens embed a `{{regex}}`.
- `ADDITIONAL_COMPILE_FLAGS` (455 lines, plus 127 in the `(feature)` form),
  `FILE_DEPENDENCIES` (22), `ALLOW_RETRIES` (6) and `MODULE_DEPENDENCIES` (2).
- `RUN` (170 lines), all of them in the 42 `.sh.cpp` files.

The runner implements:

- the file kinds;
- the three boolean keywords, with lit's grammar;
- both forms of the flags directive;
- `FILE_DEPENDENCIES`.

It runs every test once, whatever `ALLOW_RETRIES` says: a flaky test gets a
row. Modules, `.sh.cpp` and `.gen.py` are red rows against their issues.

Features are not declared by hand:

- the dialect and target features are the runner's own;
- the configuration features are read from the built libc++'s `__config_site`,
  using `libcxx_macros.py`'s table;
- every other feature is absent.

## Tiers and cost

Each slice is one tier-carrying row in the harness's registration table.

| slice | tests | tier |
|---|---|---|
| ours | 29 | Fast |
| libc-test | 275 run and 79 compile-only | Fast |
| SingleSource C, `UnitTests` and `Regression` | 206 | Fast |
| SingleSource C `Benchmarks` | 43 | Nightly, as long-running programs under TCG (*estimate*) |
| SingleSource C++ and libc++ at `-std=c++26` | 115 and 9,228 | Nightly, from M3 |
| libc++ at `-std=c++17`, LLVM's own dialect | 9,228, minus the 2,797 whose `UNSUPPORTED` names `c++17` | Weekly |

**Measured costs:**

- Compile and link: 64 ms per libc-test case, serial on the dev host (34
  cases, 2.17 s).
- Run: 11.3 ms per case of today's corpus in a shared boot. That is
  `tests/test-durations` on hosted KVM: 118 cases, 1,334 ms.

**Estimates:**

- Fast gains about 510 run tests: about 38 s of serial compile, about 6 s of
  guest time, and about 3 boots.
- libc++ compile time is unmeasured, because no ToyOS libc++ exists. At 1–2 s
  per test, 9,228 tests cost 2.5–5 core-hours.

## Migration

Each step lists its exit, and the check that reds if the step is done wrong.

1. **Ours leaves `tinycc/`.** 29 cases (59 files) move to
   `tests/conformance/ours/`: the stems 138–160, 200–203,
   `90_static_vs_global`, `90_stdio_buffering` and `debug_float`, found by
   byte comparison with TinyCC `64552b3f`. `fred.txt` is deleted.
   *Red*: `--list` differs from before, or the count gate stops holding 256
   files under `tinycc/`.
2. **The runner lands, carrying `ours` and `tinycc` as suites.**
   *Exit*: every verdict equals the old path's on the same binaries. The old
   path is run once as the differential oracle.
   *Red*: a judge that passes on exit 0 without comparing output reds a host
   fixture.
3. **libc-test**, with its NOTICE section.
   *Exit*: Fast is green, with every failing test a row against its cause's
   issue. Today 34 of 275 cases build: 132 stop on a missing `fenv.h`, 103 on
   other compile errors, and 6 at the link.
   *Red*: a one-byte edit under `upstream/` reds the lock gate, and
   `--check-forks` answers against upstream's own objects.
4. **The SingleSource C slice.**
   *Exit*: Fast and Nightly are green.
   *Red*: a judge that drops the `exit N` line reds a fixture, and a fixture
   outside `FP_TOLERANCE` reds.
5. **The TinyCC-derived files go**, once every libc symbol a TinyCC-derived
   case links (`llvm-nm -u`) is also linked by a passing adopted test. The
   deleting PR measures this. It deletes:
   - the 256 files;
   - `tests/testcases/LICENSE`;
   - NOTICE's TinyCC section;
   - `CORPUS_POPULATIONS`;
   - the `tinycc` suite.

   The `46_grep.c` refusal stays.
   *Red*: a tracked file under `tests/conformance/` that is neither under a
   lock nor in `ours/`.
6. **libc++ and SingleSource C++**, once M3 of
   `issues/build/toyos-builds-itself.md` builds libc++.
   *Red*: the expression evaluator fails the vectors of lit's own
   `llvm/utils/lit/lit/BooleanExpression.py` tests, transcribed as the
   independent oracle; and inverting `XFAIL` reds a fixture.

Step 3's lock is the per-file ledger that
`issues/build/the-third-party-corpus-is-in-no-machine-read-ledger.md` asks for,
and step 5 removes the corpus it names.

## Architectures

- **Names are arch-neutral.** A registered name carries no architecture.
- **The runner compiles for the suite's `Arch`.** It takes the
  `Arch` from `tests/common/qemu.rs`'s `SUITE_ARCH` and passes it to clang as
  the target.
- **One-arch material says so in `suite.toml`.** That covers the
  arch-specific directories (SingleSource's `X86` and `SSE` against
  `AArch64` and `NEON`), and a red row may also name an arch.
- **Long doubles need nothing.** libc-test selects its own long-double
  paths.
- **AArch64 waits on the port.** AArch64 conformance is `Tier::Local` until
  `issues/kernel/toyos-runs-on-arm64.md`'s stage 7 runs C userland and its
  stage 8 chooses runners.
