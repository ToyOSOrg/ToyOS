---
status: open
kind: track
opened: 2026-10-28
---

# ToyOS manages its own partitions

The disk driver that drives a disk owns its table and makes every change to
it (`lib/blockserve`); diskserver routes a request to it under the badge its
connector was minted with (`toyos_blockring::wire::Badge`); `toyos_gpt::edit`
is the one GPT writer, laid in UEFI 2.10 §5.3.2's order; `/system/bin/disks`
is the command. An authorization is one action on one target, and no badge
that changes a disk opens a session or writes a byte a client chose.

Built: `disks list` (`authorize = ["tables"]`) and the designation of the
machine's one DATA partition (`authorize = ["designate:data"]`), which the
build grants only a row no login can start (`build/build.rs`), so today only
a signed test image's job holds it.

## Stages, in order

1. **Interactive authorization.** `disks` changes nothing for a person at a
   shell or over ssh: no row a login can start may hold an authorization that
   changes a disk, and none is built to fill the gap. Its exit is the
   administrative authorization every system change shares (the planning
   architect, 2026-10-28) — one userland path, not a disk-specific trust
   system — through which `disks` asks for one authorization of one action
   on one target, and the supervisor mints it. Its product choice (how the
   owner approves a system change) is the owner's.
2. **Table edits.** `Edit` (one disk's table replaced by an exact intended
   table: create, delete, retype, rename, and restoring a copy, which §5.3.2
   requires be reported) and `Init` (a fresh table on one disk). Each
   authorization names the plan it permits and is bound to the disk's state
   when it was authorized — a token the driver draws at bring-up and after
   every change it makes, compared in the turn it writes. The driver refuses
   a change to a held partition or one the running system is on
   (`--active`), writes sector-granular at the disk's true last LBA, flushes
   between the two copies, and reads both back. Unrelated partitions are
   untouched by construction; another system's are reclaimed only when the
   plan names them.
3. **Scratch disks.** `Init` with one aligned partition of
   `Guid::TOYOS_SCRATCH` replaces `toyos-metal --adopt-scratch`
   (`issues/the-scratch-disk-adopt-scratch-makes-is-not-served.md`).
4. **The installer** (`issues/storage-is-layers-and-a-role-is-a-filesystem.md`,
   stage 3) lays its layout through stage 2's library and authorization.
   `toyos-metal`'s `sgdisk` and `partx` are not this track's to replace: they
   run under a bench's Ubuntu before ToyOS boots, making the install area once
   and giving partitions 7–11 the image's GUIDs on every flash, and they leave
   with Ubuntu (`issues/the-build-runs-host-tools-outside-rust-and-qemu.md`).
