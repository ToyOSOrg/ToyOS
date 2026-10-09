---
status: open
kind: defect
opened: 2026-10-09
---

# The fork's compiler drops the exit of an inclusive range loop on AArch64

The compiler that builds every ToyOS kernel, loader and program
(`rust/` at `6d6ad8c71906`, `rustc 1.99.0-dev`, LLVM 22.1.8 from
`src/llvm-project` at `ceaf0fbb8440`) turns a correct loop over an inclusive
range that ends at `u16::MAX` into an endless one for `aarch64-unknown-toyos`,
`aarch64-unknown-none-softfloat` and `aarch64-unknown-uefi`. The source is 30
lines of safe Rust. The fault is LLVM's, and upstream has it too: no compiler
measured from LLVM 21.1.2 on compiles the shape right.

## The reproducer

`minns.rs`. `caller()` returns `true`:

```rust
#![no_std]
#[derive(Clone, Copy)]
pub enum Mediated { Kept, ReadOnly }

#[derive(Clone, Copy)]
pub enum Standing { Free, Declared(Mediated) }

fn port(standing: impl Fn(u16) -> Standing, port: u16, width: u16, write: bool) -> Result<u16, u8> {
    for port in port..=port + (width - 1) {
        match standing(port) {
            Standing::Free => {}
            Standing::Declared(Mediated::ReadOnly) if !write => {}
            Standing::Declared(Mediated::ReadOnly) => return Err(9),
            Standing::Declared(Mediated::Kept) => return Err(8),
        }
    }
    Ok(port)
}

fn standing(port: u16) -> Standing {
    match port {
        0x3F8..=0x3FF | 0x20..=0x21 | 0xA0..=0xA1 | 0x70..=0x71 | 0xCF8 | 0xCFC..=0xCFF | 0xCF9 => Standing::Declared(Mediated::Kept),
        0xB2 => Standing::Declared(Mediated::ReadOnly),
        _ => Standing::Free,
    }
}

fn probe() -> bool {
    port(standing, 0xFFFC, 4, false).is_ok() & port(standing, 0xB2, 1, true).is_err()
}
pub fn caller() -> bool { probe() }
```

`rustc --edition 2021 --crate-type lib --emit llvm-ir,asm --target <target> -C
opt-level=2 minns.rs`, with the fork's `rustc` run from the store's
`sysroots/<key>` directory, `<key>` the one a worktree's `target/.deps-stamp`
gives the ToyOS targets. Each exits 0. What `caller` became:

| target | `caller` |
|---|---|
| `aarch64-unknown-toyos` | no `ret` in its IR; frame setup, then `.LBB0_1: b .LBB0_1` |
| `aarch64-unknown-none-softfloat` | no `ret`; `.LBB0_1: b .LBB0_1` |
| `aarch64-unknown-uefi` | no `ret`; `.LBB0_1: b .LBB0_1` |
| `aarch64-apple-darwin` | no `ret`; `LBB0_1: b LBB0_1` |
| `x86_64-unknown-toyos` | `ret i1 true`; `movb $1, %al`, `retq` |
| `x86_64-unknown-none` | `ret i1 true`; `movb $1, %al`, `retq` |
| `x86_64-unknown-uefi` | `ret i1 true`; `movb $1, %al`, `retq` |

It is fragile. Without the second call in `probe`, without the private `probe`
between `caller` and the calls, or with three of the `Kept` arms gone, the same
compiler compiles it right.

The same fault in the tree's own code: `toyos-userbound/tests/firmware.rs`'s
`a_port_answers_as_its_declaration_says`, whose `(0xFFFC, Width::DWord)` case
runs `firmware::port`'s range to `0xFFFF`. On an Apple-silicon development
machine, `CARGO_INCREMENTAL=0 cargo test --locked -p toyos-userbound --test
firmware --no-run` with `RUSTUP_TOOLCHAIN` naming that sysroot directory, which
holds the fork's `aarch64-apple-darwin` libraries, exits 0, and the binary run
whole prints 20 tests `ok` and never ends (ended by PID at 120 s). Its test
function is one instruction, a branch to itself.

## Which compilers

The same test binary, built and run the same way under upstream's compilers on
the same machine, one rustup toolchain installed and removed per row. `r4.rs`
and `d1.rs` are two earlier single-file forms of the reproducer, compiled to
assembly beside each row.

| toolchain | rustc | LLVM | the test binary | `caller` in `r4.rs`, `d1.rs` |
|---|---|---|---|---|
| nightly | 1.96.0-nightly (d9563937f 2026-03-03) | 22.1.0 | exit 0, 21 passed | not compiled |
| nightly-2026-05-13 | 1.97.0-nightly (8b03437a8 2026-05-12) | 22.1.4 | exit 0 | returns |
| nightly-2026-06-17 | 1.98.0-nightly (9e2abe0c6 2026-06-16) | 22.1.7 | exit 0 | returns |
| nightly-2026-07-04 | 1.98.0-nightly (c397dae80 2026-07-02) | 22.1.8 | exit 0 | returns |
| nightly-2026-07-08 | 1.99.0-nightly (f10db292a 2026-07-07) | 22.1.8 | exit 0 | returns |
| nightly-2026-07-09 | 1.99.0-nightly (14cae6813 2026-07-08) | 22.1.8 | exit 0 | returns |
| nightly-2026-07-10 | 1.99.0-nightly (af3d95584 2026-07-09) | 22.1.8 | hung, ended at 120 s | a branch to itself in both |
| nightly-2026-07-13 | 1.99.0-nightly (77cf889bc 2026-07-12) | 22.1.8 | hung | a branch to itself in both |
| nightly-2026-07-22 | 1.99.0-nightly (0e29c21d9 2026-07-21) | 22.1.8 | hung | a branch to itself in both |
| nightly-2026-09-25 | 1.100.0-nightly (f7575a9da 2026-09-24) | 23.1.1 | hung; the test's function is `b .` | not compiled; `minns.rs`: no `ret` |
| nightly-2026-10-08 | 1.101.0-nightly (1d81eb4ad 2026-10-07) | 23.1.3 | hung; the test's function is `b .` | not compiled |

Stable 1.99.0 (LLVM 23.1.1) hangs it and stable 1.98.1 (LLVM 22.1.8) does not:
`issues/rustc-1-99-0-makes-an-endless-loop-of-a-port-test-on-apple-silicon.md`
has those rows. `nightly-2026-10-08` was the newest nightly there was. No
upstream compiler has a fix.

## The cause, as far as measured

**What changed between the last good nightly and the first bad one is
`core`, not LLVM.** `14cae681329a...af3d95584dbd` is 106 commits, none under
`src/llvm-project`, and both ends report LLVM 22.1.8. Among them is
rust-lang/rust #155114 (commit `b3c94df68bf4`, merged as `71c64160bd0f`), which
rewrote `RangeInclusive`'s `next` to step with `Step::forward_overflowing` and
keep the overflow bit in `exhausted`. With `-C no-prepopulate-passes` the
reproducer's loop differs between 1.98.1 and `nightly-2026-07-22` only in block
numbering and in that callee. The fork contains both commits (`git merge-base
--is-ancestor` exits 0 for each against `6d6ad8c71906`).

**That `next` is a correct program, and LLVM miscompiles it wherever it comes
from.** The reproducer with its range replaced by this iterator, so that no
compiler's `core` decides the loop's shape:

```rust
pub struct Overflowing { start: u16, end: u16, exhausted: bool }
impl Iterator for Overflowing {
    type Item = u16;
    #[inline]
    fn next(&mut self) -> Option<u16> {
        if self.exhausted || !(self.start <= self.end) {
            return None;
        }
        let (n, o) = self.start.overflowing_add(1);
        self.exhausted = o;
        Some(core::mem::replace(&mut self.start, n))
    }
}
```

| compiler | LLVM | `caller` at `-C opt-level=2` |
|---|---|---|
| 1.88.0 | 20.1.5 | right: as an executable for `aarch64-apple-darwin` it prints `true` |
| 1.91.0 | 21.1.2 | wrong, `aarch64-apple-darwin` |
| nightly, 1.96.0-nightly (d9563937f 2026-03-03) | 22.1.0 | wrong, `aarch64-apple-darwin` |
| 1.95.0 | 22.1.2 | wrong, `aarch64-apple-darwin` |
| 1.98.0 | 22.1.8 | wrong, `aarch64-apple-darwin` |
| 1.98.1 | 22.1.8 | no `ret` for `aarch64-apple-darwin` and `aarch64-unknown-none-softfloat`; the executable hangs at `opt-level` 2 and 3 and prints `true` at 0 and 1; right for `x86_64-unknown-none` |
| the fork's | 22.1.8 | no `ret` for `aarch64-unknown-toyos` and `aarch64-unknown-none-softfloat`; right for `x86_64-unknown-toyos` and `x86_64-unknown-none` |

So stable 1.98.1 compiles the tree's test right only because its `core` does
not have the loop in this shape.

**The step that goes wrong is `indvars`.** Under `nightly-2026-07-22`, with the
reproducer as an executable that prints `caller()` and `-C
llvm-args=-opt-bisect-limit`: limit 690 prints `true`, limit 691 prints
`false`, and pass 691 is `indvars` on the loop in `probe`. Its whole effect on
that function:

```
-  %or.cond.not.i.not = icmp eq i16 %iter, -1
-  br i1 %or.cond.not.i.not, label %exit, label %backedge
+  br i1 false, label %exit, label %backedge
-  %2 = add nuw i16 %0, 1
+  %2 = add nuw nsw i16 %0, 1
```

Before it the loop carries the port being checked (`%iter`, from 0xFFFC) and
the next one (`%0`, from 0xFFFD), and computes the one after in its latch as
`add nuw i16 %0, 1`. When `%iter` is 0xFFFE that add wraps to a poison value
nothing uses, because the loop leaves on `%iter == 0xFFFF` first. The exit
deleted is that one, the only exit the range's end has; later passes fold what
is left into the endless loop. Read from the diff and not established: that
`indvars` takes the `nuw` as proof the loop cannot run that long.

**Not identified:**

- the LLVM commit at fault. It lies between 20.1.5 and 21.1.2 by the stable
  compilers above; no bisection of LLVM was run;
- which earlier pass left `nuw` on that add, and whether the flag or the
  inference from it is the bug by LLVM's own rules;
- whether upstream LLVM or rust-lang/rust has a report. None was searched for,
  and none is sent for now (root `CLAUDE.md`, "Dependencies").

## How far it reaches

**Exposed: the AArch64 kernel, loader and userland**, each built for one of the
three targets above. Nothing wrong has been found in them, and nothing was
looked for.

**What is at risk** is a range `a..=b` whose end is its integer type's maximum
at run time, in the loop shape LLVM makes of it here. A loop that never reaches
the maximum loses an exit it never takes. Seen for `u16` only: the reproducer
with `u32` or `u64` for `u16`, its range ending at that type's maximum,
compiles right for `aarch64-unknown-toyos` and `aarch64-unknown-none-softfloat`
under the fork's compiler, and its `u8`, `i16` and `u32` forms run right as
executables under `nightly-2026-07-22`. One fragile reproducer says that, so
it is no bound on the fault.

**The outcomes seen** are the endless loop and, with the passes after `indvars`
withheld, a wrong value: `false` for `true`. So a wrong answer without a hang
is possible, and no test in the tree would name it.

**x86-64: 0 of 40 sources miscompiled, which is not a proof.** 39 source
variants made while reducing, judged by whether a function is left with no
`ret`, `unreachable` or `resume`, which sees the endless loop and nothing else:
18 are miscompiled for `aarch64-unknown-none-softfloat` and the same 18 for
`aarch64-unknown-toyos`, none for `x86_64-unknown-none` or
`x86_64-unknown-toyos`; the hand-written iterator's file is the fortieth.
`indvars` is not an AArch64 pass. Stable 1.99.0 compiles the tree's test right
for `x86_64-apple-darwin`.

**The x86-64 kernel's own instance of the loop has its exit.** The kernel
calls `firmware::port` once, from `kernel/src/arch/x86_64/acpi_mode.rs`'s
`port`, with a port and width the `acpi` claim's holder chooses. From `CI=true
cargo run -- --build-only` (exit 0) at `e0a61d070`, built from nothing with and
without incremental state and disassembled: a function of its own of 0x11a
bytes in the first, inlined into `acpi_mode::port` (0x13a bytes) in the second.
In both the loop counts the width's bytes down in a 16-bit register and leaves
at zero, every refusal leaves it, and no branch back is unconditional:

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

The AArch64 kernel has no instance of that call: it is under `arch/x86_64`, by
the source and not by a disassembly.

**Not measured:** any other function of the kernel, the loader or userland, on
either architecture; `aarch64-unknown-none`, `aarch64-unknown-linux-gnu` and
`x86_64-unknown-linux-gnu` under an affected compiler.

## What does not end it

- **Moving the fork to a later upstream**: upstream has the fault through
  `nightly-2026-10-08`.
- **Taking #155114 out of the fork's `core`**: `library/core` carries no delta
  (`.claude/agents/implementer.md`, "A fork"), and LLVM's fault would stay for
  any other code of the shape, as the hand-written iterator shows.
- **Rewriting `firmware::port`**: the loop is right, and it is one loop.
- **`nightly.yml`'s pin of `portability-macos` to 1.98.1**: no job installs the
  fork's compiler by a version; it is the tree's own.

## Owner and exit

Owner: the toolchain, `rust/` and its `src/llvm-project`
(`ToyOSOrg/llvm-project`, branch `toyos-rustc-22.1-2026-05-19`). Nobody holds
it.

Whoever moves the fork to a later upstream runs both measurements below under
the moved compiler before the move lands, and corrects this file by what they
show.

**Exit**, both under the compiler the tree builds with:

1. the reproducer's `caller` is `ret i1 true` for `aarch64-unknown-toyos`,
   `aarch64-unknown-none-softfloat` and `aarch64-unknown-uefi`, by the command
   above;
2. the test binary above, built with that compiler on Apple silicon without
   incremental state, exits 0;

by a fix of the `indvars` step in the fork's LLVM, written to upstream quality
with LLVM's own regression test for it, or taken from upstream once upstream
has one. The reproducer is fragile, so a compiler that merely stops making this
shape of it meets the two measurements and not the exit. The check that holds
the fix in this tree arrives with the fix, green: a host check that compiles
the reproducer with the tree's compiler for the three targets and reads
`caller`'s return reaches it, and needs no machine to run the result.
