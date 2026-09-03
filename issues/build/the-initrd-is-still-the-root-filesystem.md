---
status: open
kind: track
opened: 2026-07-29
---

# The initrd is still the root filesystem, and it costs 85 MiB for the machine's life

The boot image was meant to become one bcachefs root partition on the boot
medium, with the initrd deleted. Every other part of that landed — symbolication
reads `.symtab` off the file that backs the executable rather than off a memory
image, one named `[profile.toyos]` with an overflow-checks gate, `hosted-rustc`
off, the asset sweep — and this part did not. `kernel/src/main.rs` still mounts
the initrd as root, the bootloader still hands over `initrd_addr`, and the image
still carries only `TOYOS-BOOT` and `TOYOS-LOG`.

Until it lands, the initrd's reserved region stays reserved for the machine's
whole life.

The block layer this needed is built. A block device is a shared handle
registered under its `DeviceId`, a second device claiming that number is
refused, every consumer holds a partition view, and one page cache serves one
(device, partition) — so `PageCacheBlockIO` is a `BlockIO` over any partition of
any registered device, which is what a root partition needs. A machine whose
only disk is the internal one now mounts `/boot` and `/log` off it
(`internal_disk_boot`).

What is left is the partition itself: a third GPT entry `TOYOS-ROOT` carrying
the image the initrd carries today, in the tree's **current** format — the
plumbing is format-agnostic and swapping the format is
`issues/kernel/bcachefs-crate-is-not-bcachefs.md`'s work, not this one; the root
GUID in the kernel arguments; and the deletion of the initrd adapter and its
slice-backed `BlockIO`. Whatever replaces the medium answers to `Profile::Metal`
first, and its defining claim still holds: it carries no virtio device anywhere
(`tests/common/qemu.rs`), so a virtio-blk root is not a shape that profile can
take.

**PR 2 also closes an incoherence PR 1 left unreachable.** On an internal-disk
boot the page cache is opened over the *whole* device while `/boot` and `/log`
read the same platter through their own eight-block `FatDevice` caches, so the
two are mutually incoherent by construction. Nothing reaches it today: the only
cache reader of that device is `open_home`, which reads block 0 and finds a
GPT'd image foreign, so `/home` is a tmpfs and no cached page ever falls inside
a FAT partition. Moving the cache onto ROOT is what makes it unrepresentable
rather than merely unreached.

Two facts worth keeping when the image is next re-costed:

- **No binary this project produces has a `.debug_*` section** — `toyos-ld`
  drops them — so `.symtab`/`.strtab`, 32.2 % of every shipped binary, *is* the
  whole debug weight and cannot be the part left behind.
- The boot medium is USB mass storage, which makes every `cargo test` the only
  coverage the USB storage path has. Swapping the dev loop to `virtio-blk`
  deletes that coverage silently.
