---
status: open
kind: defect
opened: 2026-10-10
---

# The program loader refuses clang and LLD, whose ELF tables pass one kernel allocation

A spawn reads each table the executable's dynamic section names into one
kernel heap allocation, and refuses the binary with `ResourceExhausted` when a
table passes `mm::MAX_HEAP_ALLOC`, 2 MiB less a page, 2,093,056 bytes
(`kernel/src/loader/mod.rs`, `read_elf_table`); the relocation index it builds
is held to the same bound (`kernel/src/elf/index.rs`,
`RelocationIndex::with_capacity`). std reports the refusal as "out of memory".

The clang and `ld.lld` that `cargo run -- --hosted-clang` builds for
`x86_64-unknown-toyos` (`src/hostedclang.rs`, product `6079d1a299fd8083`) are
both refused at spawn by `hosted_clang_hello` (`tests/common/hostedclang.rs`),
on the kernel's lines:

```
spawn: /system/share/hosted-clang/bin/clang: DT_RELASZ declares 5044488 bytes, past one kernel allocation
spawn: /system/share/hosted-clang/bin/ld.lld: DT_STRSZ declares 2813680 bytes, past one kernel allocation
```

The loader logs only the first table it refuses. What each binary declares,
read with `readelf -d -S`:

| | clang | ld.lld |
|---|---|---|
| `DT_RELASZ` (all `R_X86_64_RELATIVE`) | 5,044,488 | 1,958,616 |
| `.dynstr` (`DT_STRSZ`) | 5,737,227 | 2,813,680 |
| `.dynsym` | 1,674,696 | 912,072 |
| relocation index, 16 bytes a `RELATIVE` entry | 3,362,992 | 1,305,744 |

Not measured: how far `LLVM_ENABLE_PLUGINS=OFF`, which stops clang exporting
its symbols, shrinks `.dynstr` and `.dynsym`; it cannot shrink clang's
`DT_RELASZ` or its index, which count relative relocations. Packing them as
`DT_RELR` would, and the loader reads no `DT_RELR`
(`issues/the-program-loader-runs-a-binary-whose-relr-table-it-never-reads.md`).

Owner: M2 of `issues/toyos-builds-itself.md`.

Exit: `hosted_clang_hello` gets past both spawns, and a table size a file
declares still cannot make the kernel allocate past a bound the loader states.
