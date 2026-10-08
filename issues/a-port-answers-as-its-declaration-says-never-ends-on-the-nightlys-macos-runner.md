---
status: assigned
kind: defect
opened: 2026-10-08
---

# `a_port_answers_as_its_declaration_says` never ends on the nightly's macOS runner

`toyos-userbound/tests/firmware.rs`'s `a_port_answers_as_its_declaration_says`
has never finished in `nightly.yml`'s `portability-macos`, the one job that
runs the host suite on macOS, and the job was cancelled each time, which
concludes the whole nightly `cancelled`.

| run | head | job | what its log shows |
|---|---|---|---|
| 37740449787 | `9ef866436` | 113189702343 | success; the tree has no `tests/firmware.rs` yet (#749 adds it) |
| 37758546165 | `b6bcb9691` | 113249048160 | the first nightly with the test: cancelled in the step `cargo run -- --ci host` |
| 37778826093 | `8a883a142` | 113317209315 | cancelled in the same step by the job's `timeout-minutes: 350` |
| 37778826093 | `8a883a142` | 113317209042 (`host`, ubuntu-24.04, x86-64) | the same test `ok`, the binary's 20 tests `finished in 0.00s` |

Job 113317209315, a `macos-26-arm64` image, `rustc 1.99.0 (b940084d7
2026-09-28)` installed by its rustup step, in the driver's step `the
workspace's host members`:

```
14:58:17.9360280Z      Running tests/firmware.rs (target/debug/deps/firmware-078b69485f28905b)
14:58:17.9562020Z running 20 tests
   (19 lines, each `... ok`, the last at 14:58:17.9686300Z)
14:59:18.1133730Z test a_port_answers_as_its_declaration_says has been running for over 60 seconds
18:35:20.4006380Z ##[error]The operation was canceled.
18:35:22.6558800Z Terminate orphan process: pid (10800) (firmware-078b69)
```

## What reading rules out

- **The driver and cargo.** The process the runner found still alive is the
  test binary, and libtest names the one test of its 20 that had not ended.
- **A wait in the test's own code.** The crate is `#![no_std]` and
  `#![forbid(unsafe_code)]`, and the test calls `firmware::port` with a
  function of its own and nothing else: no lock, no file, no clock, no thread.
- **A panic or an overflow check.** Either ends the test.
- **The policy's source.** `firmware::port` has one loop,
  `for port in port..=port + (width.bytes() as u16 - 1)`, and the test's
  arrays are fixed. On x86-64 the same source ends.

## What was measured: it does not hang on an Apple-silicon Mac

`a_wide_access_is_held_to_every_port_it_spans`, in the same binary, calls the
same function and ended on the runner; its accesses that reach port `0xFFFF`
are refused as `PortSpan` above the loop, so the test that hangs is the only
one that runs the loop over a range whose inclusive end is `0xFFFF`. Every
host test is built at `opt-level = 2` (root `Cargo.toml`, `[profile.dev]`).
That pointed at the code `rustc 1.99.0` makes of that loop for
`aarch64-apple-darwin`. Two measurements on an Apple-silicon Mac, macOS
27.0.1, say otherwise.

The test alone, at `809c33c0c`'s tree, `cargo +<toolchain> test --locked -p
toyos-userbound --test firmware --no-run` and then the binary with `--exact
a_port_answers_as_its_declaration_says`:

| toolchain | profile | build | the test |
|---|---|---|---|
| `rustc 1.98.1 (48a229cea 2026-09-01)` | the tree's | exit 0 | exit 0, `ok`, `finished in 0.00s` |
| `rustc 1.99.0 (b940084d7 2026-09-28)`, LLVM 23.1.1 | the tree's | exit 0 | exit 0, `ok`, `finished in 0.00s` |
| `rustc 1.99.0 (b940084d7 2026-09-28)` | `opt-level = 0` | exit 0 | exit 0, `ok`, `finished in 0.00s` |

The job's own build, in a worktree at `8a883a142` under `rustc 1.99.0`: the
step's cargo line as job 113317209042 printed it, `cargo test --workspace
--exclude toyos-build --exclude ...`, with `--locked` and `--test firmware
--no-run`, so the same packages are selected and `toyos-userbound` and what
it depends on get the features and profile the step gives them; exit 0. Then
the `firmware` binary whole, its 20 tests on libtest's threads, from
`toyos-userbound/`:

| libtest's threads | runs | result |
|---|---|---|
| the default | 20 | 20 exit 0, none past its 5-minute bound |
| `RUST_TEST_THREADS=3` | 20 | 20 exit 0, none past its 5-minute bound |

So the job's tree, compiler, invocation and neighbours make a binary that
ends, 40 runs of 40, on this architecture. What is left is the machine: a
hosted `macos-26-arm64` image, macOS 26.6.2 (25G83), in a virtual machine.

Nothing the test calls reaches the machine: `firmware::port` and the test's
`standing` are arithmetic and two matches, with no system call, clock,
entropy, port I/O or thread of their own. What does reach it is libtest
around the test: the thread it starts for each test, the capture of that
thread's output, and the channel its result comes back on. libtest's line
says only that no result had come back, not that the test's body was still
running, so a thread that never started or never returned its result reads
the same in the log as a loop that never ends.

## The one instrumented run, and what follows it

The measurement that is left can only be made on the runner. `src/ci.rs`'s
`heard` ends a cargo that has said nothing for 15 minutes: it sends the
step's process group `SIGQUIT`, then `SIGKILL` to what is left 10 s later,
and reds the step with the last line said, which here is libtest's line
naming this test. macOS writes a report of a process `SIGQUIT` ends under
`~/Library/Logs/DiagnosticReports`, with the stack of every thread: measured
on the Mac above with a test binary that spins, started as `heard` starts a
step, whose report held libtest's main thread waiting for the result and the
test's own thread inside the test, and whose output ended in the millisecond
of the signal. `portability-macos`
uploads that directory as the artifact `macos-crash-reports` when the job
fails. A report of the `firmware-*` binary with its threads' stacks says
which it is: a thread inside `firmware::port`, a thread inside libtest or
std, or no test thread at all.

Assigned: the orchestrator. Before #778 lands he dispatches `nightly.yml` on
its branch and reads `portability-macos`: the job concludes `failure` and not
`cancelled`; `the workspace's host members` is red about 16 minutes after
libtest's line, `said nothing for 900s and was ended with its process group
by SIGQUIT`, the last it said that line; the steps after it run and the
driver prints its summary; no `Terminate orphan process ... firmware-*` at
the job's end; and the artifact holds a `firmware-*` report, or the upload
warns that it found none.

The reading is made before #778 lands and what it selects is part of #778,
so `main` never carries this test red on macOS under the bound:

- **The report names the cause**: it is fixed at its owner, and this file is
  deleted with the nightly that shows `portability-macos` green.
- **There is no report, or it does not name the cause**: the test is deleted
  from `toyos-userbound/tests/firmware.rs`, and this file stays, recording the
  commit that restores it and the instrument still owed, a debugger attached
  on the runner before the signal. `heard`'s `SIGQUIT` leg and the workflow's
  upload step are then deleted with it if the artifact held no report of any
  process: they are kept only for what they have been seen to write.

Owner of the test: the `acpi` claim's author (#749).

**Exit**: `portability-macos` green in a nightly on `main` with this test in
the tree, its cause named in the commit that got it there.
