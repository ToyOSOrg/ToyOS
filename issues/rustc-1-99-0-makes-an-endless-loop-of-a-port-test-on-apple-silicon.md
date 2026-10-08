---
status: assigned
kind: defect
opened: 2026-10-08
---

# rustc 1.99.0 makes an endless loop of `a_port_answers_as_its_declaration_says` on Apple silicon

`toyos-userbound/tests/firmware.rs`'s `a_port_answers_as_its_declaration_says`
never ended in `nightly.yml`'s `portability-macos`, the one job that runs the
host suite on macOS, from the first nightly that had the test (#749): the
compiler that job installed, `rustc 1.99.0 (b940084d7 2026-09-28)`, LLVM
23.1.1, compiles the test's function to one instruction, a branch to itself.

## What the runner showed

Run 37834436612, a dispatch of `nightly.yml` on #778's branch, whose driver
ends a silent step's process group with `SIGQUIT` and whose macOS job keeps
the reports macOS writes of that. `portability-macos` concluded `failure`,
with one step of 77 red:

```
21:56:02 test a_port_answers_as_its_declaration_says has been running for over 60 seconds
22:11:02 [ci] the workspace's host members: cargo test --workspace ...: said nothing for 900s and was ended
         with its process group by SIGQUIT; the last it said: test a_port_answers_as_its_declaration_says
         has been running for over 60 seconds
```

The report of the `firmware-*` binary has two threads. `main` is parked in
libtest's `run_tests_console`, in `recv` on the channel a finished test
reports on. The thread named `a_port_answers_as_its_declaration_says` has its
program counter at offset 0 of the test's own function (the closure's
`FnOnce::call_once`), its link register in libtest's caller of it, and as
inlined frames at that one address `standing` (`tests/firmware.rs:495`),
`firmware::port` (`src/firmware.rs:344`, the `match` inside its loop) and the
test's line 513, the loop over the ports nothing declared.

## The same binary, made here

On an Apple-silicon Mac, in the worktree: `cargo +<toolchain> test --locked -p
toyos-userbound --test firmware --no-run` and then the binary whole, ended
after 60 s if it had not ended:

| toolchain | environment | profile | the binary |
|---|---|---|---|
| 1.99.0 | `CI=true` | the tree's | hung: 20 tests `ok`, then libtest's line for this one |
| 1.99.0 | `CARGO_INCREMENTAL=0` | the tree's | hung, the same |
| 1.99.0 | `CARGO_INCREMENTAL=0` | `opt-level = 1` | exit 0 |
| 1.99.0 | `CARGO_INCREMENTAL=0` | `opt-level = 0` | exit 0 |
| 1.98.1 | `CARGO_INCREMENTAL=0` | the tree's | exit 0 |
| 1.99.0 | neither | the tree's | exit 0: the test alone, and 40 runs of 40 of the binary whole at the job's own tree and package selection |
| 1.98.1 | neither | the tree's | exit 0, the test alone |

Cargo builds without incremental compilation where `CI` is set, which a
hosted runner sets and a developer's shell does not: that is why the first
measurements here, made without it, found nothing. The hung binary's test
function is at the offset the runner's report names, and `otool -tv` shows
it whole:

```
..._8firmware38a_port_answers_as_its_declaration_says0...FnOnce...call_once...:
0000000100001564	b	..._8firmware38a_port_answers_as_its_declaration_says0...call_once...
```

With the test's cases edited and the hanging build repeated: without
`(0xFFFC, Width::DWord)` the binary exits 0, with it and without `(0xFFFF,
Width::Byte)` it hangs. So the compiler concludes that `firmware::port`'s
`for port in port..=port + (width.bytes() as u16 - 1)`, inlined with
`standing` over the four ports `0xFFFC..=0xFFFF`, never ends, and drops
everything after it. The source ends: the crate forbids `unsafe`, an
inclusive range that ends at `u16::MAX` is what `RangeInclusive` exists to
get right, and every other build above runs it to its end. A single file
with the loop, the match and the cases does not reproduce it under 1.99.0 at
`-C opt-level=2`; the crate boundary and the test's other cases are part of
what the optimiser needs.

Not measured: x86-64 under 1.99.0 (the Linux `host` job passes this test and
its log names no rustc version), and whether any other host code is compiled
wrongly by the same fault without hanging.

## What holds it

`portability-macos` installs `1.98.1` where it installed `stable`
(`nightly.yml`). That is a pin on a compiler nothing else in the tree pins:
the Linux jobs take the rustc of their runner's image, and a developer's
machine takes whatever its `stable` is. A developer on Apple silicon with
1.99.0 who runs `cargo run -- --ci host` with `CI` set, or without
incremental compilation, gets the step ended after 15 silent minutes with
this test named. The kernel is not built by this compiler: it takes the
fork's.

`firmware::port` is not changed for it. The fault is the compiler's, the
loop is right, and a compiler that drops a loop's exit here is not made safe
by rewriting the one loop where it was seen.

Assigned: the orchestrator. Before #778 lands he dispatches `nightly.yml` on
its branch and reads `portability-macos`: success, with `test
a_port_answers_as_its_declaration_says ... ok` in `the workspace's host
members`. He reads the same in the first nightly on a `main` that has #778.

Owner of the pin: whoever next changes `portability-macos`'s rustup step.

**Exit**: a stable rustc later than 1.99.0 under which the second row of the
table exits 0 on Apple silicon, and `portability-macos` back on `stable`,
green in a nightly on `main`.
