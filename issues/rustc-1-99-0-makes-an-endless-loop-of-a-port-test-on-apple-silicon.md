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

On an Apple-silicon Mac, in the worktree: `cargo test --locked -p
toyos-userbound --test firmware --no-run` under the toolchain and then the
binary whole, ended by PID if it had not ended, after 60 s in the first seven
rows and after 120 s in the rest:

| toolchain | its LLVM | environment | profile | the binary |
|---|---|---|---|---|
| 1.99.0 | 23.1.1 | `CI=true` | the tree's | hung: 20 tests `ok`, then libtest's line for this one |
| 1.99.0 | 23.1.1 | `CARGO_INCREMENTAL=0` | the tree's | hung, the same |
| 1.99.0 | 23.1.1 | `CARGO_INCREMENTAL=0` | `opt-level = 1` | exit 0 |
| 1.99.0 | 23.1.1 | `CARGO_INCREMENTAL=0` | `opt-level = 0` | exit 0 |
| 1.98.1 | 22.1.8 | `CARGO_INCREMENTAL=0` | the tree's | exit 0 |
| 1.99.0 | 23.1.1 | neither | the tree's | exit 0: the test alone, and 40 runs of 40 of the binary whole at the job's own tree and package selection |
| 1.98.1 | 22.1.8 | neither | the tree's | exit 0, the test alone |
| the fork's, `rustc 1.99.0-dev` | 22.1.8 | `CARGO_INCREMENTAL=0` | the tree's | **hung**, the same; build exit 0 |
| `nightly-2026-07-22`, `1.99.0-nightly (0e29c21d9 2026-07-21)` | 22.1.8 | `CARGO_INCREMENTAL=0` | the tree's | hung, the same; build exit 0 |
| `nightly-2026-09-25`, `1.100.0-nightly (f7575a9da 2026-09-24)` | 23.1.1 | `CARGO_INCREMENTAL=0` | the tree's | exit 0 |
| 1.99.0, `--target x86_64-apple-darwin` | 23.1.1 | `CARGO_INCREMENTAL=0` | the tree's | exit 0, run under Rosetta: `21 passed` |

The fork's toolchain is the one that builds the kernel and userland. A host
cargo is run against it as the build runs it: `RUSTUP_TOOLCHAIN` names the
store's `sysroots/<key>` directory, `<key>` the one the worktree's
`target/.deps-stamp` gives `x86_64-unknown-toyos`; that directory holds the
fork's `rustc` and its `aarch64-apple-darwin` libraries.

Cargo builds without incremental compilation where `CI` is set, which a
hosted runner sets and a developer's shell does not: that is why the first
measurements here, made without it, found nothing. The hung binary's test
function is at the offset the runner's report names, and `otool -tv` shows
it whole:

```
..._8firmware38a_port_answers_as_its_declaration_says0...FnOnce...call_once...:
0000000100001564	b	..._8firmware38a_port_answers_as_its_declaration_says0...call_once...
```

The binary the fork's toolchain made has the same function, one branch to
itself, at `0x100001624`. The x86-64 binary 1.99.0 made has it as a return
(`objdump -d`, the Xcode tools' LLVM one, as `otool` is):

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
Width::Byte)` it hangs. So the compiler concludes that `firmware::port`'s
`for port in port..=port + (width.bytes() as u16 - 1)`, inlined with
`standing` over the four ports `0xFFFC..=0xFFFF`, never ends, and drops
everything after it. The source ends: the crate forbids `unsafe`, an
inclusive range that ends at `u16::MAX` is what `RangeInclusive` exists to
get right, and every other build above runs it to its end. A single file
with the loop, the match and the cases does not reproduce it under 1.99.0 at
`-C opt-level=2`. Why is unknown: that file differs from the test in its
crate boundary, a `u16` where the test has the `Width` enum, three `Standing`
arms for five, no `write` and none of the test's other cases, and none of
those was isolated.

## How far it reaches

**The fork's compiler has the fault.** It is what builds the kernel and
userland, and on `aarch64-apple-darwin` it makes the same endless loop of
this test. So the fault is not LLVM 23's: three compilers of the 1.99
generation have it, two of them on LLVM 22.1.8, the LLVM under which 1.98.1
is right. Which of them first had it, and what in rustc or in its LLVM
changed, is unknown. `nightly-2026-09-25` compiles the test right; whether
that is a fix or the fault missing this function there is unknown too.

**The kernel's own instance of the loop has its exit.** The kernel calls
`firmware::port` once, `kernel/src/arch/x86_64/acpi_mode.rs`'s `port`, with a
port and a width the `acpi` claim's holder chooses. From `CI=true cargo run
-- --build-only` (exit 0) the kernel for `x86_64-unknown-none` was built from
nothing twice by the fork's compiler and its instance disassembled with the
same `objdump`:

- As that command builds it here, with incremental state (the cargo the build
  runs does not turn it off for `CI`): `firmware::port::<acpi_mode::port::{closure}>`
  is a function of its own, 0x11a bytes.
- With `CARGO_INCREMENTAL=0` beside `CI=true` (exit 0, no incremental state
  written): it is inlined into `acpi_mode::port`, 0x13a bytes.

In both the loop counts the width's bytes down in a 16-bit register and
leaves when it reaches zero, and every refusal leaves it; no branch back is
unconditional. The second, where `r12d` is the port and `r13w` the bytes
left:

```
eef55:	inc	r12d
eef58:	dec	r13w
eef5c:	je	0xeef93 <+0xb3>        ; every port asked: the access is made
eef5e:	mov	esi, 0x1
eef63:	mov	edi, r12d
eef66:	call	<kernel::arch::x86_64::pio::standing>
        ...                              ; a refusal jumps out of the loop, a pass back to eef55:
eef79:	je	0xeef55 <+0x75>
eef8d:	je	0xeef55 <+0x75>
```

That is one function of one kernel, read. The `aarch64` kernel has no
instance: the call is under `arch/x86_64`, by the source and not by a
disassembly.

**Not known, and nothing in the tree would show it:** whether the fork's
compiler makes this mistake in any other function of the kernel or userland,
for `x86_64` or for `aarch64`, the architecture it was seen on, without a
hang that names it. No test drives a dword at port 0xFFFC through the
kernel's instance, and none would catch another loop compiled this way.

**x86-64 under 1.99.0 compiles this test right** (the table's last row and
the disassembly above), on `x86_64-apple-darwin`; `x86_64-unknown-linux-gnu`
was not built. Not measured: whether 1.99.0 compiles any other host code
wrongly without hanging.

No upstream report is sent for now (root `CLAUDE.md`, "Dependencies").

## What holds it

`portability-macos` installs `1.98.1` where it installed `stable`
(`nightly.yml`). That is a pin on a compiler nothing else in the tree pins.
`guest.yml`, and so `guest / suite` and the nightly's `tcg / suite`, and
`nightly.yml`'s `portability-linux` install `stable` and log its version:
the `tcg / suite` of run 37778826093 logged `rustc 1.99.0 (b940084d7
2026-09-28)`, on x86-64. The two `host` jobs, `ci.yml`'s and `nightly.yml`'s,
install nothing and take the rustc of their runner's image. A developer's
machine takes whatever its `stable` is: one on Apple silicon with 1.99.0 who
runs `cargo run -- --ci host` with `CI` set, or without incremental
compilation, gets the step ended after 15 silent minutes with this test
named. Whether the host's toolchain is pinned once for every job is
`issues/the-host-job-runs-the-toolchain-the-runner-ships.md`'s to decide,
and this is a second measured case for it.

The pin does nothing for the fork's compiler, which no job installs by a
version: it is the tree's own.

`firmware::port` is not changed for it. The fault is the compiler's, the
loop is right, and a compiler that drops a loop's exit here is not made safe
by rewriting the one loop where it was seen.

Assigned: the orchestrator, who holds the pin and its exit too. Before #778
lands he dispatches `nightly.yml` on its branch and reads
`portability-macos`: success, with `test
a_port_answers_as_its_declaration_says ... ok` in `the workspace's host
members`. He reads the same in the first nightly on a `main` that has #778.
At each stable release he reruns the table's second row under it.

Whoever moves the fork to a later upstream runs the table's second row under
the moved toolchain, as its eighth row was run, before the move lands.

**Exit**: a stable rustc later than 1.99.0 under which the table's second
row exits 0 on Apple silicon, and `portability-macos` back on `stable`,
green in a nightly on `main`; and the fork on an upstream under which its
eighth row exits 0.
