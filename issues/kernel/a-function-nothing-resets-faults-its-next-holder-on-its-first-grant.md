---
status: open
kind: defect
opened: 2026-09-25
---

# A function nothing resets faults its next holder on that holder's first grant

T14 run 132, `lanswapcase` on `wt/toyos-logd` `b9435b98`; the defect is on
`main` too — neither `kernel/src/pcidev/mod.rs` nor netd's claim order differs
there.

`kernel/src/pcidev/mod.rs`'s `release` of the I219 (00:1f.6) resets it by
nothing — no Express capability, no AF capability, `No_Soft_Reset` set — and
unmaps every grant the dying netd held. The replacement netd maps the BAR, runs
`toyos_i219::quiesce` and asks for its grant; the grant starts bus mastering,
and the part at once writes a frame into the dead netd's buffers:

```
[22.183 cpu7] iommu: domain6 maps 0xc200000..0xc400000 at 0x2000200000
[22.183 cpu0] iommu: unit3 fault recording overflowed
[22.183 cpu0] iommu: DMA FAULT owner=slot0 unit3 stream=00:1f.6 addr=0x000000200006e000 access=write reason=0x05 domain=6 bme=cleared ...
```

`0x200006e000` is inside the dead netd's grant at `0x2000000000`. The quiesce
ran first (`userland/netd/src/i219.rs`, before `dma_alloc`) and did not stop
it: no register write retracts a frame the function had already taken in, and
bus mastering, cleared at the release, was what held it. `bme=cleared` is the
fault handler's own stop, not the state the write was made in.

The fault is counted against the new holder: its claim is marked faulted and
its bus mastering cleared, so the replacement drives a part that can neither
write a frame nor raise a message. Run 130 (`b194ec62`, the same code) took no
frame in that window and passed.

**Exit condition**: a release that resets nothing leaves the next claim of that
function reaching nothing its domain refuses, gated by a QEMU arm that releases
a part with its receive unit on under a declined reset and faults without the
fix.
