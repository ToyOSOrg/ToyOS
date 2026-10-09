---
status: assigned
kind: defect
opened: 2026-10-09
---

# The fork's LLVM deletes a loop's exit on a no-wrap flag ScalarEvolution gives the wrong value

The compiler that builds every ToyOS kernel, loader and program
(`rust/` at `6d6ad8c71906`, `rustc 1.99.0-dev`, LLVM 22.1.8 from
`src/llvm-project` at `ceaf0fbb8440`) turns a correct loop of safe Rust into an
endless one. Measured: an inclusive range that ends at `u16::MAX`, for
`aarch64-unknown-toyos`, `aarch64-unknown-none-softfloat` and
`aarch64-unknown-uefi`; and one that ends at `u128::MAX`, for
`aarch64-unknown-toyos` and `x86_64-unknown-toyos`. The fault is in LLVM's
ScalarEvolution and is the same on every target: it moves a no-wrap flag from
an increment whose wrapped result nothing uses onto a value that is used, and
`indvars` deletes the loop's exit on it. Upstream LLVM has the fault and has
merged no fix. Its open report llvm/llvm-project#175729 has the same symptom
on another loop: that ToyOS's loop is that report's fault is this file's
reading, and that the report is the known fault is a maintainer's
"probably" ("Upstream" below, "Not established").

## The fault, with no front end

`m1.ll`. `@f` returns `true` after four iterations:

```llvm
define i1 @f() {
entry:
  br label %header
header:
  %next = phi i16 [ -3, %entry ], [ %nextnext, %latch ]
  %iter = phi i16 [ -4, %entry ], [ %next, %latch ]
  %done = icmp eq i16 %iter, -1
  br i1 %done, label %exit, label %latch
latch:
  %nextnext = add nuw i16 %next, 1
  br label %header
exit:
  ret i1 true
}
```

The `add` wraps when `%next` is -1. Its result is then poison, and nothing
uses it: the loop leaves on `%iter == -1` in the next header. `opt
-passes=indvars -S m1.ll`, with the `opt` of LLVM 22.1.8 that
`nightly-2026-07-22`'s `llvm-tools` ships, writes nothing to stderr and
leaves nothing of `%done`: the header is

```llvm
header:                                           ; preds = %latch, %entry
  br i1 false, label %exit, label %latch
```

and the function never returns. The same with no `target datalayout`, with
AArch64's and with x86-64's. No `opt` built from the fork's LLVM was run; the
fork's source has the lines step 3 below reads, unchanged.

A fixed `opt` leaves `%done` as it is, `icmp eq i16 %iter, -1` with the
header's branch on it, or folds it to something under which `@f` still returns
`true`. It never leaves `br i1 false`.

Controls, each through the same `opt -passes=indvars`. The exit is folded to
`false` in: `m1` with a second exit in the latch that calls an opaque function
(`m2`); that at `i8`, `i32` and `i64`; and that with a start that is not
constant but carries `range(i16 10, 100)`. It is not folded in: `m2` without
the `nuw`; `m2` with the `add` in the header, before the rotation; and `m2`
with a start of unknown range. `m2` and the three that do not fold gave the
same under each of the three data layouts. Of eleven passes tried on `m2`
(`indvars`, `loop-unroll`, `loop-reduce`, `loop-vectorize`, `loop-idiom`,
`loop-deletion`, `loop-predication`, `loop-flatten`, `irce`,
`constraint-elimination`, `nary-reassociate`) only `indvars` leaves `br i1
false`.

## The cause

Four steps. The first two are sound, the third is the fault, the fourth is
where it shows. From `-print-changed` traces of the `u16` reproducer below,
its unoptimised IR for `aarch64-apple-darwin` through that `opt -O2` and the
fork's compiler's own for `aarch64-unknown-toyos`, which agree; LLVM source
lines are the fork's at `ceaf0fbb8440`.

1. **CorrelatedValuePropagation, on `port`, marks the range's increment
   `nuw`.** Measured: the `add i16 %iter, 1` before that pass's dump is `add
   nuw i16` after it. Read from the source: the add's only use is the header
   phi along the back edge, that edge implies `iter != 0xFFFF`, and
   `LazyValueInfo.cpp:1821` (`getValueAtUse`) reasons from exactly that. Sound:
   where `iter` is `0xFFFF` the add is poison and nothing uses it.
2. **LoopRotate, on `probe` after `port` is inlined with the constant start
   `0xFFFC`, makes that add a header phi's increment.** Measured: after it the
   loop is `m1`'s, `%next` from -3, `%iter` from -4, and `add nuw i16 %next, 1`
   in the latch. Sound: the add wraps in the iteration where `%iter` is
   `0xFFFE`, and the loop leaves before the poison is used.
3. **ScalarEvolution gives `%next` the recurrence `{-3,+,1}<nuw>`: the
   fault.** Measured: its own printed analysis of `m2` and of the real loop
   copied by hand says `{-3,+,1}<nuw><nsw>` with
   the unsigned range `[-3,0)`, and for `m2` beside it `exit count for header:
   i16 3`, the iteration at which that recurrence is 0. Read from the source:
   `ScalarEvolution.cpp:5776-5783` (`createSimpleAffineAddRec`; the general
   path at 5879-5909 does the same) copies the increment's flags onto the
   phi's recurrence without a condition, where only the post-increment
   expression is guarded by `isAddRecNeverPoison` (5798). The flag is right of
   `%next` alone, which is poison exactly where the recurrence wraps; a
   ScalarEvolution expression is uniqued without its flags, so every value
   with that expression takes it.
4. **`indvars` asks whether `%iter == -1` can hold and is told no.**
   Measured: `-C llvm-args=-opt-bisect-limit` under `nightly-2026-07-22`, the
   reproducer as an executable that prints `caller()`: limit 690 prints
   `true`, 691 prints `false`, and pass 691 is `indvars` on the loop in
   `probe`, whose whole effect on that function is

   ```
   -  %or.cond.not.i.not = icmp eq i16 %iter, -1
   -  br i1 %or.cond.not.i.not, label %exit, label %backedge
   +  br i1 false, label %exit, label %backedge
   -  %2 = add nuw i16 %0, 1
   +  %2 = add nuw nsw i16 %0, 1
   ```

   Later passes fold what is left into the endless loop. Read from the source
   and not measured, the path inside: `SimplifyIndVar.cpp:275`
   (`eliminateIVComparison`) calls `evaluatePredicateAt`;
   `ScalarEvolution.cpp:11490` takes `getMinusSCEV(iter, -1)`, which is
   `{-4,+,1} + 1`, the node `{-3,+,1}<nuw>` of step 3; its range excludes
   zero, so the compare is "known" false. `%iter + 1` is a defined value, 0 in
   the last iteration. Upstream's report traces the same calls on its own
   loop.

**What put the loop in the tree's test in that shape is `core`, not LLVM.**
`a_port_answers_as_its_declaration_says` is compiled right by
`nightly-2026-07-09` and wrong by `nightly-2026-07-10`.
`14cae681329a...af3d95584dbd` is 106 commits, none under `src/llvm-project`,
and both ends report LLVM 22.1.8. Among them is rust-lang/rust #155114 (commit
`b3c94df68bf4`, merged as `71c64160bd0f`), which rewrote `RangeInclusive`'s
`next` to step with `Step::forward_overflowing` and keep the overflow bit in
`exhausted`. With `-C no-prepopulate-passes` the reproducer's loop differs
between 1.98.1 and `nightly-2026-07-22` only in block numbering and in that
callee. The fork contains both commits (`git merge-base --is-ancestor` exits 0
for each against `6d6ad8c71906`). That `next` is a correct program.

## The reproducers

`minns.rs`, the `u16` form. `caller()` returns `true`:

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

`c_u128.rs`, the `u128` form. `caller()` returns `true`:

```rust
#![no_std]
#[derive(Clone, Copy)]
pub enum Mediated { Kept, ReadOnly }
#[derive(Clone, Copy)]
pub enum Standing { Free, Declared(Mediated) }
fn port(standing: impl Fn(u128) -> Standing, port: u128, width: u128, write: bool) -> Result<u128, u8> {
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
fn standing(port: u128) -> Standing {
    match port {
        0x38..=0x3F | 0x20..=0x21 | 0xA0..=0xA1 | 0x70..=0x71 | 0xC8 | 0xCC..=0xCF | 0xC9 => Standing::Declared(Mediated::Kept),
        0xB2 => Standing::Declared(Mediated::ReadOnly),
        _ => Standing::Free,
    }
}
fn probe() -> bool {
    port(standing, <u128>::MAX - 3, 4, false).is_ok() & port(standing, 0xB2, 1, true).is_err()
}
pub fn caller() -> bool { probe() }
```

`rustc --edition 2021 --crate-type lib --emit llvm-ir,asm --target <target> -C
opt-level=2 <file>`, each exit 0, with the fork's `rustc` run from the store's
`sysroots/8618c089fa736cb0`, by the report of the agent that ran them: no log
records the path. That is the key the worktree's `target/.deps-stamp` gave
`aarch64-unknown-toyos` and `x86_64-unknown-toyos` at `e52e6275b`, and every
row below was made from it. The stamp gives `aarch64-unknown-none-softfloat`,
`aarch64-unknown-uefi`, `x86_64-unknown-none` and `x86_64-unknown-uefi` another
key, `dc7c468f0c07446e`, which no row here was made with: whoever repeats a
row for one of those four from the key the kernel and loader take has a
different library set than the row had. The `core` crate hash in the fork's
compiler's pass traces for the two ToyOS targets is the one in the two ToyOS
`libcore` files under `8618c089fa736cb0`.

What `caller` became, `minns.rs`:

| target | `caller` |
|---|---|
| `aarch64-unknown-toyos` | no `ret` in its IR; frame setup, then `.LBB0_1: b .LBB0_1` |
| `aarch64-unknown-none-softfloat` | no `ret`; `.LBB0_1: b .LBB0_1` |
| `aarch64-unknown-uefi` | no `ret`; `.LBB0_1: b .LBB0_1` |
| `aarch64-apple-darwin` | no `ret`; `LBB0_1: b LBB0_1` |
| `x86_64-unknown-toyos` | `ret i1 true`; `movb $1, %al`, `retq` |
| `x86_64-unknown-none` | `ret i1 true`; `movb $1, %al`, `retq` |
| `x86_64-unknown-uefi` | `ret i1 true`; `movb $1, %al`, `retq` |

And by width: `c_u128.rs` with `u128` replaced by each type, `--emit llvm-ir`,
`caller` read in the IR. The `u16` row is that file's `u16` form, which
differs from `minns.rs` in `standing`'s arms and in writing the start as
`<u16>::MAX - 3`:

| type | `aarch64-unknown-toyos` | `x86_64-unknown-toyos` |
|---|---|---|
| `u8` | `ret i1 true` | `ret i1 true` |
| `u16` | **no `ret`: a block that branches to itself** | `ret i1 true` |
| `u32` | `ret i1 true` | `ret i1 true` |
| `u64` | `ret i1 true` | `ret i1 true` |
| `u128` | **no `ret`: a block that branches to itself** | **no `ret`: a block that branches to itself** |
| `i16` | 41 lines with one `ret`, not `ret i1 true`; whether it is right was not checked | the same |

The `i8` form does not compile (its literals are out of range).

Both sources are fragile. Without the second call in `probe`, without the
private `probe` between `caller` and the calls, or with three of the `Kept`
arms gone, the same compiler compiles `minns.rs` right.

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
has those rows. Five nightlies from `nightly-2026-07-10` on were run, the five
in the table, and each hangs it; no other was. `nightly-2026-10-08` was the
newest there was.

**A compiler that passes the test escapes by the shape its `core` gives the
loop, and has the fault.** The `u16` reproducer with its range replaced by
this iterator, so that no compiler's `core` decides the loop's shape:

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
| 1.88.0 | 20.1.5 | right, by shape: as an executable for `aarch64-apple-darwin` it prints `true`. In the loop that runs to `u16::MAX` the `overflowing_add` is a call of `llvm.uadd.with.overflow.i16` in every dump of its trace, so there is no `add` for step 1 to mark. It says nothing of LLVM 20.1.5's ScalarEvolution |
| 1.91.0 | 21.1.2 | wrong, `aarch64-apple-darwin`; in its trace the increment is an `add`, and `add nuw` after CorrelatedValuePropagation on `port` |
| nightly, 1.96.0-nightly (d9563937f 2026-03-03) | 22.1.0 | wrong, `aarch64-apple-darwin` |
| 1.95.0 | 22.1.2 | wrong, `aarch64-apple-darwin` |
| 1.98.0 | 22.1.8 | wrong, `aarch64-apple-darwin` |
| 1.98.1 | 22.1.8 | no `ret` for `aarch64-apple-darwin` and `aarch64-unknown-none-softfloat`; the executable hangs at `opt-level` 2 and 3 and prints `true` at 0 and 1; right for `x86_64-unknown-none` |
| the fork's | 22.1.8 | no `ret` for `aarch64-unknown-toyos` and `aarch64-unknown-none-softfloat`; right for `x86_64-unknown-toyos` and `x86_64-unknown-none` |

So 1.88.0 and 1.98.1 each pass something by a shape, the first this iterator
and the second the tree's test. Which LLVM change between 20.1.5 and 21.1.2
made the intrinsic an `add` that early is not determined, and is not where the
fault is.

## Upstream

Read from GitHub's API without credentials on 9 October 2026:

- **llvm/llvm-project#175729**, "[SCEV] Long-standing miscompile due to
  absence of per-use flags in SCEV expressions": open, opened 13 January
  2026, labelled `miscompilation`. Its loop is another, with a `nuw` the loop
  vectoriser left; its symptom is this one, `opt -passes=indvars` folding the
  exit to `false`, and its reporter's trace runs through
  `eliminateIVComparison`, `getMinusSCEV` and `isKnownNonZero`.
- **llvm/llvm-project pull request #118959**, "[SCEV] Don't blindly transfer
  nowrap flags to pre-inc addrec": open, a draft, unmerged, opened 6 December
  2024, 22 months before that reading, last updated 26 March 2026. Its body
  says "Test updates incomplete". It changes `ScalarEvolution.cpp` (+73 −37)
  and `ScalarEvolution.h` (+10 −4) and 22 test files. It is in no LLVM
  release.
- rust-lang/rust: no report of this was found by six searches.

**Not established:**

- **that #175729 is the known fault.** A maintainer's sentence there is "The
  nuw is fine as the value is never used. I've only glanced at it, but this is
  probably the known issue where we incorrectly unconditionally transfer
  nowrap flags for preinc addrecs from IR to SCEV", and on 17 February 2026:
  "I believe" that pull request "is the fix for this issue. However, it has
  some problematic impact";
- **that ToyOS's loop is #175729's.** Nobody upstream has seen it: that is
  this file's reading, from the same symptom and the same source path;
- **that #118959 fixes either.** Nobody has built an LLVM with it, here or by
  anything upstream's thread says. By reading, it drops the flag in `m2` and
  in the real loop;
- **when.** Waiting for upstream has no date.

The fork's `src/llvm-project` is one shallow commit, so its history answers
nothing; its source has no `canPreservePreIncAddRecNoWrapFlags`, the name
#118959 adds.

No report or pull request is sent for now (root `CLAUDE.md`, "Dependencies").

## How far it reaches

**Exposed: the kernel, the loader and userland, on both architectures.**
Nothing wrong has been found in them, and nothing was looked for.

**An inclusive range to its type's maximum is one instance of a class.** The
class is a loop in which an increment's wrapped result is dead, so that step 1
may mark it; a later rotation makes that increment a header phi's; the phi's
start has a known range; and a client of ScalarEvolution reasons about another
value with the same expression. The hand-written iterator above is in it
without `core`'s range, and `m1` without a range at all. A loop of the class
that never reaches the wrap loses an exit it never takes.

**Why AArch64 showed it for `u16` and x86-64 did not**, read from one pair of
traces of `minns.rs` under the fork's compiler. For `x86_64-unknown-toyos`,
`indvars` changes `port`'s loop before the run of CorrelatedValuePropagation
that marks the add for AArch64: it rewrites the exit test onto the
incremented value, the add has a second use, and no dump in the trace has
`nuw` on the `i16` increment. For `aarch64-unknown-toyos` the trace has no
`indvars` change on `port`; read from the source, `IndVarSimplify.cpp:964`
refuses that rewrite for a counter whose width is not `DL.isLegalInteger`, and
16 is not in AArch64's `n32:64` where x86-64's layout is `n8:16:32:64`. So the
width decides whether the shape is reached, and nothing else: `u128`, legal on
neither, is wrong on both, by steps 1, 2 and 4 in the x86-64 trace of it. `u8`
on AArch64, not legal either, came out right in this one source, and `m2` at
`i8`, `i32` and `i64` folds: no width is safe.

**x86-64: 0 of 40 other sources miscompiled, and one counterexample.** 39
source variants made while reducing, judged by whether a function is left with
no `ret`, `unreachable` or `resume`, which sees the endless loop and nothing
else: 18 are miscompiled for `aarch64-unknown-none-softfloat` and the same 18
for `aarch64-unknown-toyos`, none for `x86_64-unknown-none` or
`x86_64-unknown-toyos`; the hand-written iterator's file is the fortieth. None
of the forty names an integer wider than 64 bits, so by the paragraph above
each had a legal counter on x86-64; that is read for `minns.rs` and inferred
for the other thirty-nine. The counterexample is the `u128` row. Stable 1.99.0
compiles the tree's test right for `x86_64-apple-darwin`.

**The outcomes seen** are the endless loop and, with the passes after `indvars`
withheld, a wrong value: `false` for `true`. So a wrong answer without a hang
is possible, and no test in the tree would name it.

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

**Not measured:**

- **any other function of the kernel, the loader or userland, on either
  architecture.** This is the larger gap, and no reading closes it: the class
  is not `..=` loops, nor narrow integers, nor one architecture. Today's
  compiler counts nothing that would: `-C llvm-args=-stats` prints nothing
  from the fork's `rustc`, `-debug-only=indvars` is refused as an unknown
  argument, and read from the source `IndVarSimplify.cpp` emits no
  optimisation remark, while the counter the fold increments, `NumElimCmp`
  (`SimplifyIndVar.cpp:46`), counts every legitimate elimination with it. The
  measurement owed is in the exit;
- the `u128` form for `x86_64-unknown-none`, `x86_64-unknown-uefi`,
  `aarch64-unknown-none-softfloat` and `aarch64-unknown-uefi`, the targets of
  the kernel and the loader;
- code generation's loop strength reduction, which reads ScalarEvolution too,
  past `opt -passes=loop-reduce` on `m2`;
- `aarch64-unknown-none`, `aarch64-unknown-linux-gnu` and
  `x86_64-unknown-linux-gnu` under an affected compiler.

## What does not end it

- **Moving the fork to a later upstream**: the newest nightly there was hangs
  the test, and upstream's LLVM has merged no fix.
- **Waiting for upstream**: it has no date. The proposed fix has been open and
  unmerged for 22 months.
- **Taking #155114 out of the fork's `core`**: `library/core` carries no delta
  (`.claude/agents/implementer.md`, "A fork"), and the fault would stay for
  any other code of the class, as the hand-written iterator shows.
- **Rewriting `firmware::port`**: the loop is right, and it is one loop.
- **A change to `indvars`** that makes the reproducers return: `indvars` asks
  a question and is answered wrongly, and every other client of
  ScalarEvolution would go on being answered so.
- **`nightly.yml`'s pin of `portability-macos` to 1.98.1**: no job installs the
  fork's compiler by a version; it is the tree's own.

## Owner and exit

Held by the toolchain, `rust/` and its `src/llvm-project`
(`ToyOSOrg/llvm-project`, branch `toyos-rustc-22.1-2026-05-19`). A fix in the
fork's LLVM is being built on `wt/toyos-scevfix`.

Whoever moves the fork to a later upstream runs the measurements below under
the moved compiler before the move lands, and corrects this file by what they
show.

**Exit**: ScalarEvolution in the fork's LLVM no longer gives a value a no-wrap
flag that holds only where another value is poison, by a change to
ScalarEvolution and to no client of it, measured under the LLVM and the
compiler the tree builds with by all of:

1. `m1.ll` above through that LLVM's `opt -passes=indvars -S`: the output has
   no `br i1 false`, and `@f` returns;
2. `caller` is `ret i1 true` by the command above: for `aarch64-unknown-toyos`
   and for `x86_64-unknown-toyos`, in `minns.rs` and in `c_u128.rs`, from the
   key `target/.deps-stamp` gives the ToyOS targets; and for
   `aarch64-unknown-none-softfloat` and `aarch64-unknown-uefi`, in `minns.rs`,
   from the key it gives those two, the one the kernel and the loader take,
   which is not the key the table's rows for them were made from
   (`8618c089fa736cb0`, the ToyOS targets');
3. the test binary above, built with that compiler on Apple silicon without
   incremental state, exits 0;
4. the tree's own functions read: the fix behind an LLVM option, the tree
   built twice with the one compiler, the option given and withheld through
   `-C llvm-args`, and the two builds compared function by function. The
   functions that differ are a superset of those the fault changed; one that
   loses an exit branch in the build without the fix is a hit, and each hit is
   named in the pull request that closes this file. The same option is the
   fix's negative control.

Items 2 and 3 rest on shapes `rustc` may stop making, so a compiler that
merely stops making them meets those two and not the exit; item 1 has no front
end in it, and item 4 is the only one that reads the code ToyOS ships. The
check that holds the fix in this tree arrives with the fix, green.
