---
status: open
kind: defect
opened: 2026-08-28
---

# image_fits_user_half tests only the span, so an ELF whose lowest p_vaddr sits above USER_VM_BASE underflows `USER_VM_BASE - vaddr_min` and halts the machine

`kernel/src/loader/mod.rs:404` computes the rebase offset as a plain subtraction:

```rust
let base = USER_VM_BASE - layout.vaddr_min;
```

`USER_VM_BASE` is `0x100_0000_0000` (`kernel/src/loader/mod.rs:46`) and `layout.vaddr_min` is the smallest `p_vaddr` the file declares. Nothing before that line bounds one against the other.

## The chain

- `kernel/src/loader/mod.rs:391` parses the header with `elf::parse_layout`, which is `Layout::parse` plus one ceiling on the section-header table (`kernel/src/elf/mod.rs:37-46`) — nothing about vaddr magnitudes.
- `toyos-elf/src/layout.rs:166-192` sets `vaddr_min` to the plain minimum of `p_vaddr` over `PT_LOAD` and `vaddr_max` to the maximum of `p_vaddr + p_memsz`. The only arithmetic guard is the per-segment `checked_add(phdr.memsz)` at `:183-184`. The crate is kernel-agnostic and has no notion of `USER_VM_BASE`. `Layout::parse`'s closing checks (`:242-243` for `entry`, then `PT_TLS` and `PT_DYNAMIC`) all go through `contains`, which is relative to `[vaddr_min, vaddr_max)` (`:282-285`) and therefore holds at any absolute address.
- `kernel/src/loader/mod.rs:399` is the one guard. `image_fits_user_half` (`:112-114`) is `toyos_userbound::in_user_half(USER_VM_BASE, layout.span())`, and `in_user_half` (`toyos-userbound/src/span.rs:38-43`) tests only `ptr.checked_add(len) <= USER_TOP` with `ptr` bound to the *fixed constant*. `span()` is `vaddr_max - vaddr_min` (`toyos-elf/src/layout.rs:277-279`), which is invariant under translating the whole image — it answers "is the image narrow enough", never "is `vaddr_min` low enough to rebase from".
- `kernel/src/loader/mod.rs:404` then subtracts. `0x100_0000_0000 - 0x200_0000_0000` underflows.

The doc comment at `kernel/src/loader/mod.rs:104-111` states the intent the code does not implement: it says the check catches "a large enough `p_vaddr`", but a `p_vaddr` is only caught when some *other* segment holds `vaddr_min` low and stretches the span.

## Why the crafted-ELF corpus misses it

`tests/toyos-rust-tests/src/bin/abuse_elf_loader.rs:306-311` (`load_kernel_half`) and `:317-324` (`load_covers_arena`) aim at exactly this wrap and are both refused — because both build on `base_exe` (`:268-271`), whose first `PT_LOAD` sits at vaddr 0. `vaddr_min` is 0, the span is enormous, and the span test rejects them. Every case in the corpus adds a high segment beside a low one; none moves the whole image up.

## Impact

`[profile.toyos]` sets `overflow-checks = true` (`kernel/Cargo.toml:343-348`, root `Cargo.toml:183-188`); the kernel is built with that profile (`src/build.rs:316`, `src/build.rs:1003`); and `assert_overflow_checked` (`src/build.rs:494-504`) refuses to ship an image whose kernel lacks the marker. The staged artifact carries `attempt to subtract with overflow`. So the subtraction traps rather than wrapping, and `kernel/src/main.rs:102`'s panic handler halts every CPU: a machine-wide denial of service from a file an unprivileged process wrote. It breaks "the kernel never crashes from userland" and the loader module's own header promise (`kernel/src/loader/mod.rs:1-8`) that "every number the file names is untrusted: a refusal is `SyscallError::{InvalidArgument, ResourceExhausted}`, never a panic".

## Reproduction

One crafted PIE, no privilege and no endowment:

- ELF64/LSB, `e_type = ET_DYN` (`toyos-elf/src/header.rs:71-74` refuses anything else), `e_machine = EM_X86_64`, `e_phoff = 64`, `e_phentsize = 56`, `e_phnum = 1`.
- One `PT_LOAD`: `p_vaddr = 0x200_0000_0000`, `p_memsz = 0x1000`, `p_filesz = 0`.
- `e_entry = 0x200_0000_0000`, so the entry lies inside `[vaddr_min, vaddr_max)`.

`span` is `0x1000`, so `image_fits_user_half` passes; the subtraction at `:404` underflows.

Write it and spawn it the way the abuse suite already does — `fs::write` into a userland-writable directory and `syscall::spawn` with every endowment field zero (`tests/toyos-rust-tests/src/bin/abuse_elf_loader.rs:210-235`, which writes into `/home/abuse_loader` and `/tmp/abuse_loader` at `:274-275`). `sys_spawn` carries no `demand_syscap` and no gate of any kind (`kernel/src/arch/syscall/proc.rs:33-56`, dispatched at `kernel/src/arch/syscall/dispatch.rs:177-206`): naming a path is the whole authority required.

## Fix direction

The quantity that has to be checked is the rebase offset, not the span. `in_user_half(USER_VM_BASE, span)` answers "does the image fit"; it cannot answer "can the image be moved to `USER_VM_BASE`". Give `toyos-userbound` that second predicate — the decision belongs in the pure crate where it is host-tested beside `USER_TOP`, not open-coded in the loader — and have `image_fits_user_half` refuse with `SyscallError::InvalidArgument` when `USER_VM_BASE.checked_sub(vaddr_min)` is `None`, alongside the span test it already makes. With `base + vaddr_min == USER_VM_BASE` established once, `insert_elf_regions`'s `base + layout.vaddr_min + lo` (`kernel/src/loader/mod.rs:136`) becomes an identity that cannot wrap either, and the doc comment at `:104-111` becomes true of the code beneath it.

The negative control is a new `spawn_refused` case in `tests/toyos-rust-tests/src/bin/abuse_elf_loader.rs` built from an `Elf` whose *only* `PT_LOAD` sits above `USER_VM_BASE` — not `base_exe` plus a high segment, which the span test already catches. It must panic the kernel with the whole guard reverted and return `InvalidArgument` with it in place. The independent oracle is the ELF specification's own position that `p_vaddr` is unconstrained for `ET_DYN`: nothing outside the kernel promises `vaddr_min < USER_VM_BASE`, so the kernel is the only place that can.
