---
status: open
kind: defect
opened: 2026-08-26
---

# The page size's private copies moved to `mm::PAGE_SIZE`; its inline copies did not

`issues/design-debt/four-private-page-size-constants-still-have-no-owner.md`
named four private *constants* and they are gone — `file_cache::PAGE_SIZE`,
`file_backing::BLOCK_SIZE`/`BLOCK_SIZE_U64`, `fat32_adapter::BLOCK` and
`usb_gate::BLOCK` are all `crate::mm::PAGE_SIZE` now, and `src/sourcegate.rs`
bans the three initializer spellings that would bring one back.

**The same value is still written out as an array type in nine more files**, and
a ban on `: usize = 4096` cannot see any of it:

```
$ git grep -n '\[u8; 4096\]\|0u8; 4096\|; 4096\]' -- kernel/src
kernel/src/bcachefs_adapter.rs:275,401   fn write_page(… data: &[u8; 4096])
kernel/src/elf/mod.rs:347                let mut page_buf = [0u8; 4096];
kernel/src/fat32_adapter.rs:643,1044     fn read_page(… buf: &mut [u8; 4096])
kernel/src/gpt.rs:273,286                buf: [u8; 4096]
kernel/src/loader/mod.rs:86              let mut page_buf = [0u8; 4096];
kernel/src/nvme_gate.rs:42,82            let mut buf = vec![0u8; 4096];
kernel/src/page_cache.rs:62,72           fn raw_block_read(… buf: &mut [u8; 4096])
kernel/src/process.rs:1933               let mut page_buf = [0u8; 4096];
kernel/src/tmpfs.rs:25,123               fn read_page(… buf: &mut [u8; 4096])
kernel/src/vfs.rs:106,551-554            fn write_page(… data: &[u8; 4096])
```

## How this was measured, and what it means

Moving `mm::PAGE_SIZE` to `8192` and running `cargo check --target
x86_64-unknown-none` from `kernel/` is the test: with the four constants pinned,
none of them errors, and the tree still refuses to compile — nine errors, every
one of them a mismatch against an inline `[u8; 4096]`:

```
E0053 method `read_page` has an incompatible type for trait   src/tmpfs.rs:25
E0053 method `read_page` has an incompatible type for trait   src/fat32_adapter.rs:643
E0308 mismatched types                                        src/tmpfs.rs:29
E0308 mismatched types                                        src/file_backing.rs:128 (vs page_cache.rs:62)
E0308 mismatched types                                        src/vfs.rs:569
E0308 mismatched types                                        src/elf/mod.rs:351
E0308 mismatched types                                        src/process.rs:1940
E0308 mismatched types                                        src/loader/mod.rs:97
```

So the compiler already ties these to each other — this is not a silent
divergence class, and that is why it is design debt rather than a hazard. What
it costs is that `mm::PAGE_SIZE` is not yet the one place the value lives:
changing it is nine compile errors in nine files rather than one edit.

**Not all nineteen sites are the page size.** `gpt.rs`'s buffer is an LBA
staging area and `nvme_gate.rs`'s two are a gate's scratch; whoever does this
decides per site rather than sweeping the literal, exactly as the constants
issue asked.

`FileBacking::read_page`'s signature is the natural anchor: it is the one the
trait impls in `tmpfs.rs`, `fat32_adapter.rs` and `file_backing.rs` all have to
agree with, and `file_backing::BLOCK_SIZE` already names the value it wants.
