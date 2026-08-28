---
status: open
kind: defect
opened: 2026-08-28
---

# `DeviceInfo::mint` installs before the call can commit, so a refused describe and a superseded reset both strand handles nobody can name

`DeviceInfo::mint` (`kernel/src/object/device.rs:47`) writes handles into the caller's own table as it goes and has no undo. The Framebuffer arm installs three — `scanout[0]`, `scanout[1]`, `cursor` — through three `?`s at `:53`, `:54` and `:56`, each an `install_buffer` (`:36-43`) that turns `HandleTable::install`'s `TableFull` into `ResourceExhausted`. Nothing preflights room and nothing takes back what already went in: the local `info` copy holding the fresh `RawHandle`s is dropped on the `?`, and the entries stay in `table`.

The table already offers the preflight this wants — `has_room`, *"whether `n` more `install`s can all succeed"* (`kernel/src/object/handle.rs:177`) — called at `kernel/src/arch/syscall/ipc.rs:387` and `kernel/src/loader/start.rs:169` and nowhere else. The all-or-nothing standard is the table's own too: `HandleTable::transfer` restores every entry when its sink refuses, because *"the caller was told nothing happened"* (`handle.rs:339-341`).

**A stranded handle is unnameable, and therefore unclosable.** Its number is computed only inside `mint` and dies with the dropped `info`; the caller gets an error word. Guessing is not a recovery: `sys_close` (`kernel/src/arch/syscall/handles.rs:45`) reaches `HandleTable::remove` (`handle.rs:286-302`), a miss — empty slot or wrong generation — is `BadHandle`/`Stale`, and `refuse` routes that to `process::handle_fault` (`kernel/src/process.rs:1668`), a diverging `exit`. The first wrong probe kills the prober. The slots come back only when the process dies and `close_all` drains the table (`kernel/src/object/ops.rs:172`).

Two callers reach `mint`, and they strand handles by different routes.

**1. `describe` on a nearly-full table** (`device.rs:115-132`). `read_device` calls it on every `SYS_READ` of a Framebuffer or Nic claim (`kernel/src/object/ops.rs:308-310`). With one or two free slots the first installs succeed and the last refuses: those entries are now stranded, and `described.bytes` — the only place a minted description is remembered — stays `None` at `:121-125`. So `info_read` is never set and the next `SYS_READ` re-runs `mint`: free a slot, read again, strand it again, until the table is garbage. `sys_read`'s own comment says this path "runs once per device per boot" (`kernel/src/arch/syscall/io.rs:130-132`); a refusal is exactly the case where it does not.

**2. A superseded `remint`** (`device.rs:135-145`) — and this one needs no full table. `sys_gpu_reset_scanout` mints at `kernel/src/arch/syscall/device.rs:143`, three fresh `SharedMemObject`s over the new scanout (`kernel/src/device.rs:121-127`) and three fresh handles, and only then windows the output buffer at `:150`, answering `BadAddress` at `:151` if it cannot. `UserAddr::checked` in the dispatch (`kernel/src/arch/syscall/dispatch.rs:417`) proves only that the address is in the user half (`kernel/src/mm/mod.rs:71`); the mapping check is `window` (`kernel/src/user_ptr.rs:269-287`), after the mint. One such failure is still recoverable — `remint` did store the new bytes at `device.rs:143`, and a later `SYS_READ` hands them over. The *next* mode set is not: it overwrites `described.bytes`, and the previous batch's three numbers are gone. A mode set that answers `BadAddress` followed by any further mode set therefore strands three handles per lost batch, until the table is full and case 1 takes over.

## What it costs

The claimant's table is 4096 slots (`toyos-abi/src/handle.rs:20-25`); each stranded entry costs one permanently, plus the `SharedMemObject` it pins — `over` allocates nothing (`kernel/src/object/shm.rs:60-74`), so the memory cost is the object, not the framebuffer. Nothing crosses a process boundary: the table is the caller's own (`process::with_process_data` → `data.handles`), so this is a self-DoS of the claimant and not an isolation break. What it is, is the kernel half-committing a change to a process's table and then reporting failure — the thing `has_room` and `transfer` exist to prevent.

## Precondition

Only a device claimant reaches either path: `SYS_DEVICE_CLAIM` demands a `SysCap` carrying `Rights::DEVICE` (`kernel/src/arch/syscall/device.rs:101`), and `SYS_GPU_SET_RESOLUTION` demands a Framebuffer claim handle (`dispatch.rs:412`). The framebuffer claim is exclusive (`kernel/src/device.rs:43-50`, `:92-97`), so that is the compositor and nobody else. Case 1 additionally needs the claimant's table within two slots of full at the first read of the claim; case 2 needs only a mode set whose `info_out` is an unmapped user-half address, followed by another mode set. Only the Framebuffer arm has the multi-install window — `Nic`, `Hda` and `VirtioSound` install once each (`device.rs:59-73`) and so fail clean. Nothing covers this today: `abuse_handle_table` (`tests/toyos-rust-tests/src/bin/abuse_handle_table.rs:1-18`) names the three insertion paths it checks — plain open, `dup2`, `SYS_SPAWN`'s slot map — and this is not one of them.

## Fix direction

Make the mint all-or-nothing, and make it the last thing the syscall does before it can no longer fail. `has_room(3)` before the first `install_buffer` closes case 1 with the table's own primitive; a `mint` that removes what it installed on the way out closes it without a preflight. For case 2 the write-back length is not actually unknown — `FramebufferInfo` is a padding-free `repr(C)` of eight `u32`s (`toyos-abi/src/lib.rs:83-105`), so `size_of::<FramebufferInfo>()` is available before the mint and the output window can be taken first, which is what `dispatch.rs:415-417` already says it wants: *"a bad address must not leave a resolution the caller was never told about."* Either way, `SYS_GPU_SET_RESOLUTION` must not be able to replace `described.bytes` with a batch the caller was never handed.
