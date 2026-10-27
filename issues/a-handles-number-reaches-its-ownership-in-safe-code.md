---
status: open
kind: defect
opened: 2026-10-27
---

# A handle's number reaches its ownership in safe code

`RawHandle` is a `u32` anyone can name, and the handle syscalls that take one
are safe functions (`toyos_abi::syscall::close`, `dup`, `message_send`). A
device's window leaves every address space that mapped it with its last
handle (`SharedMemObject::aperture`, `kernel/src/object/shm.rs`), so
`syscall::close(bar.as_handle())` in safe code leaves a
`toyos::shm::SharedMemory` over a range the kernel has unmapped and may map
again for something else, which its next `bytes().write` then reaches.

`OwnedHandle::from_raw` is `unsafe` for this reason, and
`SharedMemory::adopt` and `lent` take an `OwnedHandle`. IPC's receive answers
the handles a frame moved into this process as `RawHandle`s
(`Connection::recv_handles_exact`, `FrameRx::take_handles`, `Batch::exact` in
`sdk/toyos/src/ipc.rs`), so every receiver that maps one claims its ownership
in an `unsafe` block of its own.

The same gap is why no SDK type can prove a region exclusive: a safe `dup` of
`as_handle()` and a safe send of the copy give a peer a writable mapping. Only
`toyos::process::Image` forms a slice over a writable region, under its own
encapsulation.

Owner: `toyos-abi` and `toyos::ipc`.

**Exit**: no safe function closes, duplicates, sends or maps a handle by
number. The handle syscalls are `unsafe` or take the SDK's owned and borrowed
handle types, IPC's receive answers `OwnedHandle`s, and each receiver's
`from_raw` goes.
