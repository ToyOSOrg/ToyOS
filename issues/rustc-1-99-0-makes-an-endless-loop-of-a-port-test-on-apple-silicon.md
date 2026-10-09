---
status: assigned
kind: tooling
opened: 2026-10-08
---

# rustc 1.99.0 makes an endless loop of `a_port_answers_as_its_declaration_says` on Apple silicon

`toyos-userbound/tests/firmware.rs`'s `a_port_answers_as_its_declaration_says`
never ended in `nightly.yml`'s `portability-macos`, the one job that runs the
host suite on macOS, from the first nightly that had the test (#749): the
compiler that job installed, `rustc 1.99.0 (b940084d7 2026-09-28)`, LLVM
23.1.1, compiles the test's function to one instruction, a branch to itself.
The job is pinned to 1.98.1 for it. The fault is LLVM's ScalarEvolution's,
on every target, and is not this compiler's alone:
`issues/the-forks-llvm-deletes-a-loops-exit-on-a-no-wrap-flag-scalar-evolution-gives-the-wrong-value.md`
has its cause, its reproducers and the compilers that have it.

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

On an Apple-silicon development machine, in the worktree: `cargo test --locked
-p toyos-userbound --test firmware --no-run` under the toolchain and then the
binary whole, ended by PID if it had not ended, after 60 s in the first seven
rows and after 120 s in the last:

| toolchain | its LLVM | environment | profile | the binary |
|---|---|---|---|---|
| 1.99.0 | 23.1.1 | `CI=true` | the tree's | hung: 20 tests `ok`, then libtest's line for this one |
| 1.99.0 | 23.1.1 | `CARGO_INCREMENTAL=0` | the tree's | hung, the same |
| 1.99.0 | 23.1.1 | `CARGO_INCREMENTAL=0` | `opt-level = 1` | exit 0 |
| 1.99.0 | 23.1.1 | `CARGO_INCREMENTAL=0` | `opt-level = 0` | exit 0 |
| 1.98.1 | 22.1.8 | `CARGO_INCREMENTAL=0` | the tree's | exit 0 |
| 1.99.0 | 23.1.1 | neither | the tree's | exit 0: the test alone, and 40 runs of 40 of the binary whole at the job's own tree and package selection |
| 1.98.1 | 22.1.8 | neither | the tree's | exit 0, the test alone |
| 1.99.0, `--target x86_64-apple-darwin` | 23.1.1 | `CARGO_INCREMENTAL=0` | the tree's | exit 0, run under Rosetta: `21 passed` |

Cargo builds without incremental compilation where `CI` is set, which a
hosted runner sets and a developer's shell does not: a build with incremental
state does not show the fault. The hung binary's test function is at the
offset the runner's report names, and `otool -tv` shows it whole:

```
..._8firmware38a_port_answers_as_its_declaration_says0...FnOnce...call_once...:
0000000100001564	b	..._8firmware38a_port_answers_as_its_declaration_says0...call_once...
```

The x86-64 binary 1.99.0 made has it as a return (`objdump -d`, the Xcode
tools' LLVM one, as `otool` is):

```
..._8firmware38a_port_answers_as_its_declaration_says0...FnOnce...call_once...:
100001620:	push	rbp
100001621:	mov	rbp, rsp
100001624:	mov	rax, rdi
100001627:	mov	qword ptr [rdi], -0x1
10000162e:	pop	rbp
10000162f:	ret
```

With the test's cases edited and the hanging build repeated: without
`(0xFFFC, Width::DWord)` the binary exits 0, with it and without `(0xFFFF,
Width::Byte)` it hangs. The loop the compiler ends wrongly is
`firmware::port`'s `for port in port..=port + (width.bytes() as u16 - 1)`,
inlined with `standing` over the four ports `0xFFFC..=0xFFFF`. The source
ends: the crate forbids `unsafe`, and an inclusive range that ends at
`u16::MAX` is what `RangeInclusive` exists to get right.

## What holds it, and what the pin is worth

`portability-macos` installs `1.98.1` where it installed `stable`
(`nightly.yml`). That is a pin on a compiler nothing else in the tree pins.

**1.98.1 has the faulty LLVM too.** It compiles this test right because its
`core` does not yet give the range's loop the shape LLVM miscompiles; given
that shape written by hand it makes the same endless loop. The pin keeps one
test of one job green. It is no statement that 1.98.1 compiles the rest of the
host suite right, and nothing measured says either way.

**No stable is known to be coming that the job can go back to.** Five
nightlies were run, of 10, 13 and 22 July, 25 September and 8 October 2026,
the last the newest there was, and each hangs the table's second row; none
between them was run. Upstream's LLVM has merged no fix. That its open report
llvm/llvm-project#175729 is of this fault is the reading of
`issues/the-forks-llvm-deletes-a-loops-exit-on-a-no-wrap-flag-scalar-evolution-gives-the-wrong-value.md`,
which lists it under "Not established".

`guest.yml`, and so `guest / suite` and the nightly's `tcg / suite`, and
`nightly.yml`'s `portability-linux` install `stable` and log its version:
the `tcg / suite` of run 37778826093 logged `rustc 1.99.0 (b940084d7
2026-09-28)`, on x86-64, where this test is compiled right for
`x86_64-apple-darwin` (the table's last row); `x86_64-unknown-linux-gnu` was
not built. The two `host` jobs, `ci.yml`'s and `nightly.yml`'s, install
nothing and take the rustc of their runner's image. A developer's machine
takes whatever its `stable` is: one on Apple silicon with 1.99.0 or later who
runs `cargo run -- --ci host` with `CI` set, or without incremental
compilation, gets the step ended after 15 silent minutes with this test named.
Whether the host's toolchain is pinned once for every job is
`issues/the-host-job-runs-the-toolchain-the-runner-ships.md`'s to decide,
and this is a second measured case for it.

`firmware::port` is not changed for it. The fault is the compiler's, the
loop is right, and a compiler that drops a loop's exit here is not made safe
by rewriting the one loop where it was seen.

Assigned: the orchestrator, who holds the pin and its exit. Before #778
lands he dispatches `nightly.yml` on its branch and reads
`portability-macos`: success, with `test
a_port_answers_as_its_declaration_says ... ok` in `the workspace's host
members`. He reads the same in the first nightly on a `main` that has #778.
At each stable release he reruns the table's second row under it.

**Exit**: `portability-macos` installs `stable` again and is green in a
nightly on `main`, which takes a stable rustc under which the table's second
row exits 0 on Apple silicon. None exists; one arrives when upstream's LLVM
has the fault fixed, or when `core` gives the range's loop another shape
again, which is how 1.98.1 passes today and fixes nothing.
