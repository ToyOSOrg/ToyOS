---
status: assigned
kind: track
opened: 2026-09-07
---

# The T14 answers only through a USB stick, and nothing a program prints reaches the harness

Every result from the bench rides a stick and a reboot into Ubuntu. The laptop
is on a cable on the same LAN as the development Mac and its NIC is the onboard
Intel I219 at `00:1f.6`, `8086:15fc`, which the kernel enumerates and nothing
claims. The track is to make that cable the answer path.

The substrate a process needs to drive a PCI function itself is built
(`kernel/src/pcidev/mod.rs`, `userland/netd/src/virtio_net.rs`). What is left is
the I219 driver in netd, with DHCP under the hostname `toyos-t14` and a first
ping and ssh from the Mac; a record stream from logd to a listener in the
harness, so a boot's log arrives while it is booting; command execution, file
transfer both ways and key auth in sshd, with the harness running userland tests
over ssh through a russh client; and a netboot spike in which the firmware
fetches the loader over HTTP so the stick leaves the boot path.

Constraints a reader would otherwise pay to re-derive:

- **The driver does not live in the kernel** (owner, 2026-09-07). The kernel owns
  the claim of the PCI function; the holder drives the registers, takes the
  interrupts and owns its DMA, which lives in that function's own IOMMU domain.
- **A machine with no IOMMU unit hands no function to a process** (owner ruling,
  2026-09-07, on the ordering ruling in
  `issues/kernel/every-driver-is-still-in-the-kernel.md`). The claim is refused
  by name, netd exits, and the machine boots on; `iommu_virtio_platform`'s
  no-unit arm is where that is read back. The T14 has VT-d, so this is not a
  bound on the bench — but a `pcidev` refusal there is the first thing to check
  before suspecting the driver.
- **ssh is the bench's transport and a real feature**: sshd is built on russh
  and the harness's client is russh too. No host ssh binary, no fork.
- **Addressing is DHCP with a hostname**, resolved through the router's DNS. The
  T14's MAC is the same under ToyOS and Ubuntu, so the lease is the one `t14`
  already resolves to. Wi-Fi is out — the AX210 needs a firmware image.
- The I219 has **32-bit BARs**, and `pcidev`'s window allocator has only ever
  placed a 64-bit one: `Refusal::NoWindow` on that machine means nothing was
  found above everything firmware described and below the platform's fixed MMIO.
- A function that advertises no PCIe function-level reset is refused
  (`Refusal::NoReset`), because nothing else can put it back into a known state
  when the process driving it dies.
- The metal loop is `toyos-metal` (`src/metal.rs`), and the T14 is run by the
  orchestrator alone.
