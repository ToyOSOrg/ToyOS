---
status: open
kind: track
opened: 2026-08-02
---

# The IOMMU translates, and does none of what it exists for

Discovery and translation are built: every unit the machine reports is
inventoried, every function `pci::enumerate` returned gets a context entry
naming one identity-mapped domain, the fault event is armed, translation is on.
A unit this kernel cannot program is left switched off with a line naming the
register that decided it, which leaves that machine exactly as it boots with no
unit at all. Three gates stand on it, two of which strand a function above and
below its context entry — the only things in the suite that can tell a
translating unit from one that is merely switched on, because under identity
mapping every device *in* the tables behaves the same either way.

Interrupt remapping is built: every source is in the remappable format, `IRE` is
set, `CFI` is never written, and every table entry is source-id-verified against
the requester id its one source really carries. Per-driver domains are built for
four functions; the refusal is not, and is next.

The gate no longer takes the unit's register-window address from the kernel. It
was taken from the `iommu: unit0 @0x…` line, and the mitigation recorded here —
"a kernel lying there would have the gate read some other page, where `GSTS`
would not show `IRES`" — was refuted by measurement: a kernel that writes the
real `GSTS`, `RTADDR` and `IRTA` values into a page of RAM and prints that
address passes every one of the five gates. The address is now the harness's own
constant, `0xfed90000`, which is where QEMU puts an `intel-iommu` on q35; the
kernel's `@0x…` is asserted equal to it and the window is required to decode as
a unit. One unit per machine is stated rather than assumed — a second
`translating` line is a refusal until the harness models two.

Of the two things this stage was told to verify rather than assume, one was
decided and one cannot be. QEMU does **not** block a compatibility-format
message once `CFI` is clear, though the specification requires it — measured, by
leaving one source at a time in that format and watching its device go on
working — so a source nobody moved keeps working here and black-screens on real
hardware. Whether the unit's own fault-event MSI is exempt is settled
by nothing in reach, the green fault gates included: QEMU sends that event
straight to the APIC without consulting the remapping path at all, so those
gates are consistent with the exemption holding and with the model blocking
nothing. Both, with the interrupt-cache invalidation that is unmeasurable for a
third reason, are recorded in
`issues/kernel/qemu-passes-compatibility-format-interrupts.md` as T14 questions.

**Domains, mapping, invalidation, faults — built.** Many domains, an address
allocator per domain whose first address sits a quarter of the way up the width
so a descriptor still carrying a physical address faults rather than lands,
map/unmap, and invalidation on every change because `CAP.CM` is set on every
unit in reach. Four functions hold one each — xHCI, NVMe, virtio-net,
virtio-console — and `iommu_domain_isolation` walks their tables host-side and
requires the physical page sets to be pairwise disjoint. `DomainId`,
`IommuError` and `DeviceSpace` earned their place; `DmaPerm` did not and is not
in the tree, because 2 MiB is the only leaf this kernel writes — coarser than
any split a driver's pools offer — and QEMU drops an access its cached
translation denies rather than recording a fault, so a narrowed mapping is
neither expressible nor observable here. `trait Iommu` still has one implementor
and still does not land.

The fault handler's bounded half is built and its terminal action is still a
halt: every stream is kernel-owned, so a driver reaching outside its domain is a
kernel bug. Before the halt, Bus Master Enable is cleared on the offending
function, the first record is latched whole, and a count is kept per unit and
per function with the domain each is in. Clearing `BME` is the ceiling on a
storm — a function that cannot master the bus raises no second fault — and the
reschedule handoff is not written, because with a halt there is nothing on the
other side of it to hand to. It lands with the process-kill arm. What is not
negotiable is that teardown's slow half never runs from the deferred
zero-handle queue.

**Nothing here measures invalidation, and the reason is not that QEMU has no
caches.** It has both: `vtd_lookup_iotlb` at `hw/i386/intel_iommu.c:2118` and
`vtd_update_iotlb` at `:2234`, and a generation-tagged context cache at `:2131`.
Only the *success* path fills the IOTLB, so a missed post-map invalidation
cannot show at all, and a missed post-unmap one would need a test that re-aims a
device at an address whose page has been reused — none exists. Measured: with
every invalidation removed, all five IOMMU gates stayed green and only
`-D warnings` noticed.

**The refusal.** Sequenced last, after the first userspace driver has moved
(`issues/kernel/every-driver-is-still-in-the-kernel.md`), because before that it
costs every machine with no vIOMMU — the default for QEMU, VirtualBox, VMware,
Parallels and essentially every cloud instance — and protects nothing that has
moved. Two rules need restating before they can be written, because as worded
they would refuse the harness's own machines: the extended-interrupt-mode rule
has to be stated in terms of the x2APIC ids actually in use rather than of
x2APIC being enabled — which is what interrupt remapping already does, since
QEMU reports `ECAP.EIM` clear and a table entry's destination is then eight
bits wide — and the isolation-scope rule refuses a device whose scope
is not a singleton — written for peer-to-peer behind a switch, and wrong for a
root-complex-integrated function, which is what both the audio and networking
targets on the T14 are. Restating it is the owner's call.

Risks that are not stage-specific:

- **The harness's virtio devices were outside the unit, and now are — measured.**
  QEMU keeps a virtio function on `&address_space_memory` unless it is created
  with `iommu_platform=on` (`hw/virtio/virtio-bus.c:86-99`), which the harness
  set nowhere; and where the host offers `VIRTIO_F_ACCESS_PLATFORM` a guest
  cannot silently decline it, because `virtio_validate_features` refuses
  `FEATURES_OK` (`hw/virtio/virtio.c:2270-2276`) and the device is lost rather
  than bypassing. Both halves are fixed and `iommu_virtio_platform` asserts the
  negotiation guest-side. virtio-sound is the one declared exception: turning
  the flag on puts every audio DMA through the unit, gate A has not judged that
  shape, and by owner ruling it stays off until it has
  (`issues/kernel/three-devices-still-reach-all-of-memory.md`).
- **Isolation scopes and reserved regions are modelled, not measured.** QEMU's
  topology is flat and publishes no RMRR; the T14 gives the first real answer,
  and it may refuse a device this project wants in userspace.
- **Cost is unmeasurable in the harness.** The 2× bar is answerable only on
  hardware, in a same-session A/B. So are cache-snooping walks, real
  access-control enforcement, and mid-DMA function reset on a real device.
