---
status: open
kind: defect
opened: 2026-08-28
---

# `place_module` rounds every static TLS module but the first to 16 bytes, so `std_tls`'s own 64-byte `PT_TLS` is placed 32 bytes off its declared alignment

## The claim

`toyos-elf::tls::place_module` gives every static TLS module after the first a 16-byte-aligned base, whatever alignment that module's `PT_TLS` declared. The kernel records each module's `p_align` and then throws all but the maximum away. A module whose declared alignment exceeds 16 is silently placed misaligned rather than refused — and one already is, in this tree, on every run.

## The mechanism

`toyos-elf/src/tls.rs:77`

```rust
pub fn place_module(cursor: usize, memsz: usize) -> Option<(usize, usize)> {
    let base = if cursor > 0 { align_up(cursor, 16)? } else { 0 };
    Some((base, base.checked_add(memsz)?))
}
```

There is no alignment parameter. Both call sites have the module's own align in hand and pass neither:

- `kernel/src/loader/tls.rs:167` — `place_module(cursor, lib.tls_memsz)`, with `lib.tls_align` folded into `max_align` one line later at `:169`.
- `kernel/src/loader/tls.rs:181` — `place_module(cursor, tls.memsz as usize)`, with `tls.align` folded in at `:183`.

`max_align` flows only into `toyos_elf::tls::plan` (`kernel/src/loader/tls.rs:45`), which aligns the *block's* `tls_start` (`toyos-elf/src/tls.rs:63`). So the module at `base_offset == 0` inherits a correct alignment for free and every later module inherits 16.

`base_offset` is the module's real address base, not bookkeeping:

- the template is copied to `block.add(plan.tls_start + module.base_offset)` — `kernel/src/loader/tls.rs:66`;
- the DTV entry is `block_phys + (plan.tls_start + module.base_offset)` — `kernel/src/loader/tls.rs:96`;
- TPOFF resolves to `base_offset + sym.value + addend - total_memsz` — `kernel/src/loader/mod.rs:798` and `:806` — and `tp = tls_start + total_memsz` (`toyos-elf/src/tls.rs:68`), so the variable lands at `tls_start + base_offset + sym.value`.

`p_align` is file-declared input, and nothing bounds it to 16. `Layout::parse` refuses only a non-power-of-two or one above `MAX_TLS_ALIGN = 2 MiB` (`toyos-elf/src/layout.rs:254`, `toyos-elf/src/lib.rs:74`). Both loaders share that parse: the exe at `kernel/src/loader/mod.rs:391`, the shared library at `kernel/src/elf/mod.rs:309`. `load_needed_libs` (`kernel/src/loader/mod.rs:662`) loads every `DT_NEEDED` module, so two or more static TLS modules is the ordinary case, not a contrived one.

The module header at `toyos-elf/src/tls.rs:10-13` states the invariant it does keep — "`data_start` must carry the largest alignment any module asked for" — and that is exactly the incomplete half: the largest alignment at the block start says nothing about where each module inside it begins. `place_module`'s own doc (`toyos-elf/src/tls.rs:72`) documents the 16 as intended, and `toyos-elf/tests/tls.rs:70-76` pins it green, so nothing in the tree reads this as a bug today.

## This is not hypothetical — the tree does it to itself

`toyos-ld` hard-codes `p_align: 64` on every `PT_TLS` it emits (`toyos-ld/src/emit_elf.rs:1056`) and aligns the TLS block start to 64 (`toyos-ld/src/emit_elf.rs:419`). Read out of the built artifacts:

| module | `p_memsz` | `p_align` |
|---|---|---|
| `tests/toyos-rust-tests/tls-lib/target/x86_64-unknown-toyos/toyos/libtls_lib.so` | 160 | 64 |
| `tests/toyos-rust-tests/target/x86_64-unknown-toyos/toyos/std_tls` | 152 | 64 |
| `.../libtls_cranelift.so` | 968 | 64 |
| `.../libtls_dlopen_lib.so` | 240 | 64 |
| `.../libtls_multi_crate.so` | 192 | 64 |

`std_tls` has exactly one `DT_NEEDED`, `libtls_lib.so`. Spawning it walks `build_tls_layout` as: lib → `place_module(0, 160)` = base 0, cursor 160; exe → `place_module(160, 152)` = base `align_up(160, 16)` = **160**, cursor 312; `max_align = 64`, so `tls_start ≡ 0 (mod 64)` and the executable's TLS module begins at `tls_start + 160 ≡ 32 (mod 64)` — 32 bytes off the alignment its own program header declares. Every thread of that process gets the same offsets: `kernel/src/process.rs:833-840` rebuilds the block from the stored `tls_modules`.

So the tree's own linker declares 64 and the tree's own loader honours 16, and the two have never been made to agree.

## Impact

Bounded, and deliberately stated narrowly. `base_offset`/`memsz` bookkeeping stays internally consistent: modules do not overlap, no other module's or process's memory is touched, the kernel does not fault, and 16 bytes still satisfies `cmpxchg16b`. This is not an isolation break and not a kernel panic.

What it is: the kernel silently violating the psABI contract it implements, for a legitimately-formed ELF, where the rule is that untrusted input is refused rather than quietly reinterpreted. Nothing in-tree currently needs more than the 32 bytes it accidentally gets, so no test is red. The moment a second-or-later module holds a thread-local needing 32- or 64-byte alignment — an `alignas(32)`/`#[repr(align(64))]` per-thread SIMD accumulator or cache-line-padded counter, the ordinary reason a `p_align` above 16 exists — that variable sits at a misaligned absolute address: silently wrong for a plain load, and a `#GP` that kills the process for an aligned SIMD load. A userland process crashing because of a kernel loader decision is the inverse of the rule this kernel holds.

## Precondition / repro

No privilege needed. An unprivileged process writes a conforming ELF to any path it can name and spawns it — the shape `tests/toyos-rust-tests/src/bin/abuse_elf_loader.rs:264,280` already uses, and `sys_spawn` (`kernel/src/arch/syscall/proc.rs:33`) is path-addressed with no capability gate. The three conditions are: two or more static TLS modules (a `DT_NEEDED` library with TLS plus an exe with TLS); the earlier module's `memsz` not a multiple of the later module's `p_align`; and a thread-local in the later module that actually needs more than 16 bytes of alignment. `std_tls` already satisfies the first two.

Not reachable through `dlopen`: `tls_alloc_block` (`kernel/src/arch/syscall/vm.rs:328`) hands each dynamic module its own `PageAlloc`, page-aligned and therefore over-aligned.

## Fix direction

Give `place_module` the module's alignment and make it a refusal, not a rounding:

```rust
pub fn place_module(cursor: usize, memsz: usize, align: usize) -> Option<(usize, usize)>
```

with `base = align_up(cursor, align.max(16))` for `cursor > 0` — `align` already established a power of two ≤ `MAX_TLS_ALIGN` by `Layout::parse`, the same fact `plan` relies on for its mask. The two call sites pass `lib.tls_align` and `tls.align as usize`, which are already read one line below each. `max_align` then genuinely bounds the block, because every module's own base is a multiple of its own align and `tls_start` is a multiple of the max. `plan`'s size arithmetic already carries one `align` addend; interior padding must be counted the same way, so `cursor` must be the post-padding total (it already is — `build_tls_layout` returns `cursor` at `kernel/src/loader/tls.rs:193`) and the block sized from it.

`toyos-elf/tests/tls.rs:70-76` pins the current 16-byte behaviour and must be rewritten, not merely extended: the assertion to keep is that for every module, `(tls_start + base_offset) % align == 0`.

Two checks this change owes. Negative control: revert the whole placement change onto the base and show a test that spawns an exe declaring a 64-aligned `PT_TLS` behind a library of `memsz=160` fails its own `assert_eq!(addr % 64, 0)` — the numbers above say it fails at 32. Independent oracle: the x86-64 psABI's variant II rule that each module's TLS block satisfies its own `p_align`, cross-checked against glibc's `_dl_allocate_tls_storage`/`_dl_next_tls_modid` placement, which rounds each module by that module's align rather than a constant. `toyos-ld/src/emit_elf.rs:1056`'s hard-coded 64 is the second half of the same disagreement and deserves a look in the same pass — a `p_align` the linker invents rather than derives from the sections is a number nothing verifies either.
