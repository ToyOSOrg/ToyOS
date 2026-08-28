---
status: open
kind: defect
opened: 2026-08-28
---

# compute_tpoff drops r_addend on the cross-module branch, so an initial-exec reference into another module's TLS resolves to the wrong offset

# `compute_tpoff` drops `r_addend` on the cross-module branch

`R_X86_64_TPOFF64`/`TPOFF32` is `S + A - tp`. The kernel's own contract comment
says so — `kernel/src/elf/reloc.rs:245` reads
`// TPOFF = base_offset + symbol_offset + addend - total_memsz (linker's convention).`
— and two of the function's three branches implement it. The third does not.

## The chain

`kernel/src/elf/reloc.rs`, `compute_tpoff`:

- `:254-255` — `r_sym == 0`: `lib_base_offset + r_addend - total_memsz`. Correct.
- `:258-259` — symbol defined in this module: `lib_base_offset + sym.value + r_addend - total_memsz`. Correct.
- `:262-265` — symbol defined in *another* module:
  `module.base_offset as i64 + sym_offset as i64 - total_memsz as i64`.
  `r_addend` is a live parameter here and is never read.

`kernel/src/loader/mod.rs`, `exe_tpoff`, is the same code a second time: `:798`
and `:806` add the addend, `:813-816` returns
`module.base_offset as i64 + sym_offset as i64 - total_memsz as i64` and does not.

The addend is carried intact all the way to that last step and only then
discarded: `CachedRelocs.tpoff64: Vec<(u64, u32, i64)>` (`kernel/src/elf/cache.rs:23-24`)
→ `typed_entries` (`reloc.rs:58-70`) → `apply_tpoff_relocs` (`reloc.rs:144-156`)
→ `compute_tpoff`. `defining_module` (`reloc.rs:191-207`) returns the symbol's raw
`st_value` via `LoadedLib::resolve_tls` (`kernel/src/elf/mod.rs:226-228`), so
nothing folds the addend in upstream either.

Three call sites reach it: `kernel/src/loader/mod.rs:761` (every startup
library), `kernel/src/loader/mod.rs:772-781` (the executable's own TPOFF tables),
and `kernel/src/arch/syscall/vm.rs:242` (`sys_dlopen`).

The sibling on the identical branch gets it right. `resolve_dtpoff`
(`reloc.rs:227-243`) returns `sym_offset as i64 + r_addend` at `:237` for the
same cross-module case. Two functions, one input shape, opposite answers — this
is a drop, not a convention.

## Impact

A wrong value, not an out-of-bounds write. The destination is bounds-checked
independently of the value by `LoadedLib::write_at` (`reloc.rs:19-46`), and the
write lands in the loading process's own private writable window
(`reloc.rs:34-41`; see `kernel/src/loader/mod.rs:726-727`). The consequence is
that the process's `%fs`-relative access to the other module's TLS variable is
off by the addend — it reads and writes the wrong bytes of its own TLS block, or
faults on a wild offset and is killed. No kernel memory, no other process, no
panic. Against a *crafted* binary this grants an attacker nothing it did not
already have; the cost falls on an honest binary from a toolchain that emits
what this one cannot.

## Precondition

An ELF carrying a `TPOFF64` or `TPOFF32` relocation whose `r_sym` names a TLS
symbol undefined in that module, defined by another module in the static TLS
block, with a nonzero `r_addend` — i.e. initial-exec access to a *field* of
another module's TLS object. Nothing refuses it on the way in:
`toyos-elf/src/rela.rs:236-250` (`validate`) bounds `r_offset + width` against
the writable window and `r_sym` against `.dynsym`, and has no opinion on the
addend or on an undefined symbol; `reloc.rs:6-8` states unresolved symbols are
logged, never fatal.

Nothing this tree builds hits it. Measured with `readelf -r` over the built
artifacts: zero `R_X86_64_TPOFF64`/`TPOFF32` relocations in all 18 executables
under `userland/target/x86_64-unknown-toyos/toyos/`, and zero in all four test
shared objects under `tests/toyos-rust-tests/` (which carry only `DTPMOD64` /
`DTPOFF64` with `r_sym == 0`); `tests/toyos-rust-tests/target/x86_64-unknown-toyos/toyos/std_tls`
is genuinely `DT_NEEDED libtls_lib.so` and its `.rela.dyn` holds 2568 `RELATIVE`
+ 46 `GLOB_DAT` and nothing else. toyos-ld also cannot express the triggering
form: `named_tpoff64s` is `Vec<(u64, String)>` with no addend field
(`toyos-ld/src/reloc.rs:21`, pushed at `:328` and `:533`), and every emit site
hardcodes `r_addend: 0` (`toyos-ld/src/emit_elf.rs:1249-1255`, `:1334-1341`,
`:1420-1427`). So this is latent until a foreign toolchain — or a hosted rustc
— produces initial-exec cross-module TLS.

## Fix direction

Add `+ r_addend` to `kernel/src/elf/reloc.rs:264` and
`kernel/src/loader/mod.rs:815`, matching `resolve_dtpoff`'s `:237`. Leave the
`None` arms alone: `loader/mod.rs:810-812` documents returning `0` there as a
deliberate refusal rather than a guess, and that is a separate decision.

Better than fixing it twice: the two functions differ only in how they fetch a
symbol (`LoadedLib::symbols()` versus `ExeTables::symbol` over the backing), so
the arithmetic belongs in one place that both call. The duplication is what let
the branch diverge in one copy and would let it diverge again.

The two checks this change owes, since it is loader/ABI work. Negative control:
a `.so` in the crafted-ELF corpus with a `TPOFF64` whose `r_sym` is an undefined
TLS symbol and whose `r_addend` is nonzero, loaded beside a library defining that
symbol, asserting the resolved slot equals `module.base_offset + sym_offset +
r_addend - total_memsz`; reverting the whole fix must fail it. Producing that
input needs an addend slot on `named_tpoff64s` in toyos-ld, or a hand-assembled
`.rela.dyn` — which is itself the reason nothing in-tree catches this today.
Independent oracle: the x86-64 psABI's definition of `R_X86_64_TPOFF64` as
`S + A - tp`, cross-read against glibc's or musl's `TPOFF` case in
`elf_machine_rela`.
