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
(`kernel/src/pcidev/mod.rs`, `userland/netd/src/virtio_net.rs`); the I219 driver
is built (`toyos-i219/`, `userland/netd/src/i219.rs`) and moves frames under
QEMU's `e1000e`; netd takes its address from DHCP rather than carrying one
written down (`userland/netd/src/dhcp.rs`); and sshd grew command execution,
file transfer both ways and key auth. What is left is the laptop:

- **The claim on the T14's own card.** `tests/lancase` is the boot that puts
  netd in front of it, and the metal loop pings the address that card holds
  across the window between the machine's two operating systems. Three runs on
  the bench: the claim was refused for want of MSI-X, `pcidev` grew MSI, and it
  now stops at the 32-bit BAR. What is owed for that is
  `issues/kernel/a-32-bit-bar-needs-the-host-bridges-aperture.md`, and until it
  is paid no process on this machine can drive that cable.
- **The record stream from the laptop**, so a boot's log arrives while it is
  booting. The guest half is built and green under QEMU on both drivers
  (`toyos-logstream/`, `userland/logd/src/stream.rs`); the metal half — arming
  the flashed image with the Mac's address and listening while the T14 boots —
  waits on the claim above.
- **The first ssh from the Mac into ToyOS on the T14**, through the harness's
  russh client (`tests/ssh-client-host`), running a test binary over the cable
  and judging its exit status. The same dependency.
- and a **netboot spike** in which the firmware fetches the loader over HTTP so
  the stick leaves the boot path.

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
  host-name option. **The name resolves to nothing on this LAN** — measured: the
  T14's DHCP-served resolvers are the ISP's, and on the development Mac `t14`
  resolves to the *Tailscale* address `100.92.92.12`, which only Ubuntu ever
  holds. So the address is read off the claimed PCI function instead
  (`Driver::wire`), the wire is `enp0s31f6` at `192.168.1.46/24` with the Mac on
  `192.168.1.47`, and the boot's own MAC record is what ties a reply to the
  boot. Wi-Fi is out — the AX210 needs a firmware image.
- **The I219 is an MSI part**, measured: `/proc/interrupts` names its interrupt
  `IR-PCI-MSI-0000:00:1f.6` and `msi_irqs/162` reads `mode=msi`. `pcidev` armed
  MSI-X alone and refused it; it arms either now.
- The I219 has a **32-bit BAR** (`bar0=0xbcf00000`), and that is where the claim
  stops today: `pcidev`'s window allocator places a BAR above everything
  firmware described, and below 4 GiB there is no above — the platform's fixed
  MMIO is at `0xFEC00000`. Leaving the BAR where it sits is not the way out
  either: the internal NVMe's `0xbce00000` is in the same 2 MiB page, which is
  the only page size this kernel maps. `survey_low_space` prints what the
  machine has left and what it cannot answer for; the owed work and its three
  prices are `issues/kernel/a-32-bit-bar-needs-the-host-bridges-aperture.md`.
- **QEMU's `virtio-net-pci-non-transitional` on `q35` advertises no PCIe
  function-level reset** — measured, not assumed: `pcidev`'s refusal on that
  ground reddened every netd registration at once. So a re-claim is made safe by
  ordering instead: bus mastering starts on the claim's **first grant**, never at
  hand-over, so a function still holding its last holder's queue addresses can
  act on none of them. `release` asks for a reset where the function advertises
  one; the I219 does, so on the T14 both hold.
- **The record stream is `logstream=<a.b.c.d>:<port>` on the parameter line**,
  copied by the kernel into `/system/bin/init`'s environment and read from there
  by `logd` (`toyos-logstream`'s `PARAM` and `ENV`). What is left to build is the
  metal half: arming the flashed image with the Mac's address and listening while
  the T14 boots. A boot that dies before `logd` runs still needs the stick.
- **A stalled peer's backpressure reaches `logd`'s queue only after megabytes.**
  Between them stand a 2 MiB kernel pipe (`kernel/src/pipe.rs`'s `PIPE_SIZE`) and
  netd's 64 KiB send buffer, and a `log-storm` at `--smp 8` produces 4,213 lines
  / 674 KiB — measured — which they absorb entirely. The guest arm for that path
  widens the storm's records instead of narrowing the peer (`log-storm-wide`).
- The metal loop is `toyos-metal` (`src/metal.rs`), and the T14 is run by the
  orchestrator alone.
