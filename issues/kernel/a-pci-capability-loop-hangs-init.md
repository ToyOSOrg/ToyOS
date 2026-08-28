---
status: open
kind: defect
opened: 2026-08-28
---

# The PCI capability walk follows the device's own next-pointer forever, and virtio's second pass indexes `mapped_bars` with a device byte the first pass had bounds-checked

`toyos-untrusted`'s module header states the class exactly: *"a `u32` a device wrote, or a `u16` a peer wrote, was carried in a plain integer and then used as an index, a length or an address."* Three sites in the PCI and virtio boot path are that shape, and `kernel/src/drivers/virtio.rs:3` already imports `Untrusted` — for the used ring at `virtio.rs:375-379`, and for nothing else in the file.

## The chain, on current main

**One: the capability walk has no termination argument.** `kernel/src/drivers/pci.rs:211-218` is the whole of `CapabilityIter::next`:

```
if self.next == 0 { return None; }
let offset = self.next as u64;
self.next = self.device.read_config_u8(offset + 1);
Some(Capability { device: self.device, offset })
```

`self.next` comes back off the device's config space every step (`Mmio::read_u8` is `read_volatile`, `kernel/src/mm/mmio.rs:51-55`). There is no visited set, no iteration cap, no requirement that links increase, and no 4-byte alignment check; `pci.rs:185` reads offset 0x34 without first testing the Status register's Capabilities-List bit. A function whose byte at `offset+1` names `offset` never yields 0, and the iterator never returns `None`. The bound at `pci.rs:222` (`MAX_DEVICES: usize = 256`) caps how many functions get enumerated, not how long one walk runs.

Five sites walk it: `pci.rs:122` and `pci.rs:166` (MSI-X and MSI arming), `hda.rs:603` (`power_up`, reached from `hda.rs:545`), and `virtio.rs:103` and `virtio.rs:121`. The first three use `.find()` and escape a cycle only if the capability they want lies inside it. The two in `virtio.rs` are `for` loops that must exhaust the iterator: on a cyclic chain they do not return, and boot wedges with no panic and no log line.

**Two: the second virtio pass drops the first pass's bounds check.** `virtio.rs:102` declares `let mut mapped_bars: [Option<crate::mm::Mmio>; 6]`. The first pass guards it (`virtio.rs:105-106`):

```
let bar_idx = cap.read_u8(4) as usize;
if bar_idx < 6 && mapped_bars[bar_idx].is_none() {
```

The second pass re-reads the same device byte at `virtio.rs:126` and indexes with it at `virtio.rs:130` — `let Some(bar) = mapped_bars[bar_idx].as_ref() else { continue };` — with the `< 6` test not repeated. The only filter between them is `if cap.id() != PCI_CAP_ID_VENDOR { continue; }` at `virtio.rs:122-124`, which says nothing about byte 4. Because both reads are volatile reads of a device-owned byte they need not even agree, so re-checking is not redundant: it is the check. `bar_idx` up to 255 into a `[_; 6]` is a raw Rust bounds panic in the kernel.

**Three: a device's offset and length reach an assert.** `virtio.rs:127-128` take `offset = cap.read_u32(8) as u64` and `length = cap.read_u32(12) as u64`, both device-written u32, and `virtio.rs:131` hands them straight to `bar.subregion(offset, length.max(4))`. `kernel/src/mm/mmio.rs:35-37` is `assert!(offset + size <= self.size, "Mmio subregion OOB: ...")`. `self.size` is the fixed `0x4000` from `virtio.rs:110-111`, a constant window rather than the BAR's advertised size. Nothing between the reads and the call compares either value against it. The sum cannot overflow u64, so this is a clean assert — a clean kernel panic on a value the device chose.

## Impact

Sites two and three panic the kernel during driver init, before the compositor. Site one is worse because it is silent: no panic, no message, no deadline, no watchdog — the boot simply stops inside a `for` loop over `capabilities()`, and nothing on the log names which function did it.

Site three is not hostile-device-only. `0x4000` at `virtio.rs:111` is a guess, not the BAR's real size, so a fully conformant virtio device whose modern BAR exceeds 16 KiB and whose capability sits above `0x4000` — a large notify region under `page-per-vq`, or enough queues — panics the boot on a legal configuration.

## Precondition

No userland reach: `grep -rni "pci" toyos-abi/src/` is empty, no syscall names a PCI function, a BAR or a capability, and `pci::enumerate` runs once at `kernel/src/main.rs:342`. Every walk runs only on a function a driver already matched — `virtio_net.rs:199`, `virtio_sound.rs:298`, `virtio_console.rs:148`, `virtio_gpu.rs:487` all select on `is_id(0x1AF4, <device id>)` before `VirtioDevice::init` reaches `VirtioPciConfig::parse` at `virtio.rs:616`; `hda.rs:545` calls `power_up` on the matched controller. The actor is a PCI function whose config space the kernel does not control and that presents itself as a device a driver claims: a passthrough or hot-plugged device, a hostile or buggy emulated device from a hypervisor, firmware that leaves a mis-linked list, or real silicon with a large virtio BAR.

To reproduce each arm, the device needs, respectively: a capability at 0x40 whose next byte at 0x41 reads 0x40; a vendor-specific capability (id 0x09) whose byte at cap+4 is at least 6; a vendor-specific capability whose u32s at cap+8 and cap+12 sum past 0x4000.

`grep -rni "capability chain|cap chain|CapabilityIter|cap_ptr"` over `tests/` and `kernel/` hits only the four definition lines in `pci.rs`. Nothing in the estate presents a malformed chain, which is why no boot has hit this.

## Fix direction

`CapabilityIter` should carry its own termination: a 256-entry visited bitmap or a hard iteration cap, plus a refusal for a link that is not 4-byte aligned or that lands below 0x40, logged once per function and naming bus/dev/func the way `pci.rs:129` already does. Bounding it inside the iterator fixes all five call sites at once, which is the argument for putting it there rather than at each caller.

`virtio.rs`'s second pass should not re-derive what the first already decided. Either keep the first pass's parse — one loop that produces a bounded `(bar_idx, offset, length)` per capability and a second that consumes it — or, if two passes stay, take the three device values through `Untrusted`: `Untrusted::new(cap.read_u8(4)).index(mapped_bars.len())` for the index, and the offset and length checked against the mapped window's size before `subregion` sees them, refused by name rather than asserted. The window size is the other half: `virtio.rs:111` should map what the BAR actually advertises, so that "past the window" means past the device's real region rather than past a constant.

`toyos-untrusted`'s own contract is what makes this mechanical — its methods take the bound and answer `Result`, so the compare cannot be forgotten. A negative control for the fix is a config-space actuator that presents each of the three shapes and asserts the kernel refuses by name and boots on: today the cycle arm hangs and the other two panic.
