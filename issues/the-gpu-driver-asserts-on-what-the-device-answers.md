---
status: open
kind: defect
opened: 2026-09-03
---

# The GPU driver asserts on what the device answers

Six `assert!`s in `kernel/src/drivers/virtio_gpu.rs` panic the kernel on a
device answer other than `RESP_OK_NODATA`, on paths userland drives: lines 294
(`RESOURCE_CREATE_2D`), 304 (`RESOURCE_UNREF`), 309 (`RESOURCE_ATTACH_BACKING`),
350 (`SET_SCANOUT`), 362 (`TRANSFER_TO_HOST_2D`) and 373 (`RESOURCE_FLUSH`) —
the first four on `set_resolution`, the last two on every `present_rect`. A
device's answer crossed a trust boundary and is refused, never asserted; the
resource-id collision the second mode change used to hit was one instance of
this class (the answer was `0x1203`, and the kernel died on it), and the fix
removed the instance, not the class.

Exit: each answer becomes a refusal to the caller by name; a device answer that
cannot be refused is a kernel bug and says so.
