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

Built and green under QEMU: the substrate (`kernel/src/pcidev/mod.rs`), the
I219 driver (`toyos-i219/`, `userland/netd/src/i219.rs`), netd's address from
DHCP (`userland/netd/src/dhcp.rs`), the record stream (`toyos-logstream/`,
`userland/logd/src/stream.rs`) and sshd's exec, transfer and key auth. What is
left is the laptop — the claim on its own card (`tests/lancase`), the stream and
the ssh from the Mac over the cable (`tests/ssh-client-host`), and a netboot
spike that takes the stick out of the boot path — and all of it waits on
`issues/kernel/a-32-bit-bar-needs-the-host-bridges-aperture.md`.

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
- **Addressing is DHCP with a hostname**, and netd sends `toyos-t14` as the
  host-name option — but **the name resolves to nothing on this LAN**, measured:
  the T14's DHCP-served resolvers are the ISP's, and on the development Mac
  `t14` resolves to the Tailscale address `100.92.92.12`, which only Ubuntu ever
  holds. The address is read off the claimed PCI function instead
  (`Driver::wire`): `enp0s31f6` at `192.168.1.46/24`, the Mac on `192.168.1.47`.
  Wi-Fi is out — the AX210 needs a firmware image.
- **The I219 is an MSI part**, measured: `/proc/interrupts` names its interrupt
  `IR-PCI-MSI-0000:00:1f.6` and `msi_irqs/162` reads `mode=msi`.
- The I219 has a **32-bit BAR** (`bar0=0xbcf00000`): `pcidev`'s window allocator
  places a BAR above everything firmware described, and below 4 GiB there is no
  above — the platform's fixed MMIO is at `0xFEC00000`. Leaving the BAR where it
  sits is not a way out either: the internal NVMe's `0xbce00000` is in the same
  2 MiB page, which is the only page size this kernel maps.
- **QEMU's `virtio-net-pci-non-transitional` on `q35` advertises no PCIe
  function-level reset** — measured, not assumed: `pcidev`'s refusal on that
  ground reddened every netd registration at once. So a re-claim is made safe by
  ordering instead: bus mastering starts on the claim's **first grant**, never at
  hand-over, so a function still holding its last holder's queue addresses can
  act on none of them. `release` asks for a reset where the function advertises
  one; the I219 does, so on the T14 both hold.
- **The record stream is `logstream=<a.b.c.d>:<port>` on the parameter line**,
  copied by the kernel into `/system/bin/init`'s environment and read from there
  by `logd` (`toyos-logstream`'s `PARAM` and `ENV`). A boot that dies before
  `logd` runs still needs the stick.
- **A stalled peer's backpressure reaches `logd`'s queue only after megabytes.**
  Between them stand a 2 MiB kernel pipe (`kernel/src/pipe.rs`'s `PIPE_SIZE`) and
  netd's 64 KiB send buffer, and a `log-storm` at `--smp 8` produces 4,213 lines
  / 674 KiB — measured — which they absorb entirely. The guest arm for that path
  widens the storm's records instead of narrowing the peer (`log-storm-wide`).
- The metal loop is `toyos-metal` (`src/metal.rs`), and the T14 is run by the
  orchestrator alone.
