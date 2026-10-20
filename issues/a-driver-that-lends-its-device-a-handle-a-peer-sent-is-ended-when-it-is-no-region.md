---
status: open
kind: defect
opened: 2026-10-20
---

# A driver that lends its device a handle a peer sent is ended when it is no region

`SYS_DEVICE_DMA_MAP` takes its region with `HandleTable::get`
(`sys_device_dma_map`, `kernel/src/syscall/device.rs`), so a handle of another
kind is `HandleError::WrongType`, which ends the caller, exit 139
(`process::handle_fault`). `HandleTable::get_sent` answers `InvalidArgument`
for it instead, and is what `SYS_SPAWN` and `SYS_NAMESPACE_BUILD` take a
handle a peer sent with (`kernel/src/object/handle.rs`).

The region a holder lends is a peer's by contract: `toyos_pci_claim::lend`
maps "`region`, which a peer lent", and reads `InvalidArgument` as
`Unlendable` (`lib/pci-claim/src/lib.rs`), a refusal the kernel never answers
for a handle of another kind. Since #953 the hda driver lends its controller
the ring soundserver sent with its acceptance, unchecked
(`offered` and `toyos_pci_claim::lend` in `system/hda/src/main.rs`): a
soundserver that sends a pipe end or a connection there ends hda rather than
being refused. diskserver is not reached, because `SharedMemory::lent` refuses
what `SYS_FSTAT` does not answer as a region before it maps one
(`system/diskserver/src/region.rs`). A region's kind is one `SYS_FSTAT` settles,
so this is not `issues/a-received-handle-has-no-knowable-type.md`'s case.

The kernel owns the fix rather than each lender: what a holder lends is a
peer's by `lend`'s contract, and the kernel already answers a peer's handle of
the wrong kind `InvalidArgument` at `SYS_SPAWN` and `SYS_NAMESPACE_BUILD`; a
check before every lend would repeat that in each driver.

Owner: the kernel's device calls (`kernel/src/syscall/device.rs`).

**Exit**: `SYS_DEVICE_DMA_MAP` takes its region through `get_sent`, and
`HandleError::WrongType`'s comment names it beside `SYS_SPAWN` and
`SYS_NAMESPACE_BUILD`.
