---
status: open
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
- **A wait on anything.** The crate is `#![no_std]` and
  `#![forbid(unsafe_code)]`, and the test calls `firmware::port` with a
  function of its own and nothing else: no lock, no file, no clock, no thread.
  What does not end is a computation.
- **A panic or an overflow check.** Either ends the test.
- **The policy's source.** `firmware::port` has one loop,
  `for port in port..=port + (width.bytes() as u16 - 1)`, and the test's
  arrays are fixed. On x86-64 the same source ends.

## What reading does not rule out

`a_wide_access_is_held_to_every_port_it_spans`, in the same binary, calls the
same function and ended on macOS. Its accesses that reach port `0xFFFF` are
refused as `PortSpan` above the loop. The test that hangs is the only one that
runs the loop over a range whose inclusive end is `0xFFFF`: `(0xFFFC, DWord)`
and `(0xFFFF, Byte)`. Every host test is built at `opt-level = 2` (root
`Cargo.toml`, `[profile.dev]`). That points at the code `rustc 1.99.0` makes of
that loop for `aarch64-apple-darwin`, and nothing has measured it: the log
says which test, not which iteration. The `host` job's log names no rustc
version, so whether x86-64 passed under the same compiler is not known either.
No run of the test on an arm64 Mac, under any compiler, is on record: #749's
and every later pull request's host suite ran on CI's Linux.

## The one measurement

On an arm64 Mac, the test alone, built three ways, each run ended after 60 s
if it has not ended by itself, with a stack sample of one that had not:

1. the tree's profile under `rustc 1.98.1`;
2. the tree's profile under `rustc 1.99.0`;
3. `opt-level = 0` under `rustc 1.99.0`.

`cargo +<toolchain> test -p toyos-userbound --test firmware --no-run`, then
the binary with `--exact a_port_answers_as_its_declaration_says`. Arm 2 alone
hanging, its sample inside `firmware::port`, is the compiler's code for that
loop; all three ending moves the question to the runner.

Until then the driver ends the step: `src/ci.rs`'s `heard` kills a cargo that
has said nothing for 15 minutes with everything it started and reds the step
with the last line said, which here is libtest's line naming this test, and
the job goes on to its other steps. The nightly stays red on macOS, in
minutes and by name.

Owner: the `acpi` claim's author (#749), whose test it is.

**Exit**: `portability-macos` green in a nightly on a `main` that still has
this test, the cause named in the commit that gets it there.
