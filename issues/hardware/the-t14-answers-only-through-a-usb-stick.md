---
status: assigned
kind: track
opened: 2026-09-07
---

# The T14 answers only through a USB stick

Every result from the bench rides a stick and a reboot into Ubuntu. The laptop
is on a cable on the same LAN as the development Mac and its NIC is the onboard
Intel I219 at `00:1f.6`, `8086:15fc`, which netd claims and drives
(`toyos-i219`). The track is to make that cable the answer path.

Built and read on the T14 (`lan_talk`, `lan_swap`): netd leases as
`toyos-t14`, the Mac finds `toyos-t14.local` and reads the boot's log from its
first line while it runs, pings it, runs a command over ssh under the image's
key and hands the machine back with `reboot`, and a service is swapped over ssh
without a reboot. Every LAN metal row but the delivery probe rides the one
talking boot. What is left is file transfer both ways and a refused key on the
T14, which sshd serves and only QEMU has read; the harness running userland
tests over ssh through a russh client, which saves a flash only on a boot held
open past the runner's bound
(`issues/hardware/a-swap-on-the-t14-lives-inside-a-metal-boots-bound.md`); and
a netboot spike in which the firmware fetches the loader over HTTP so the stick
leaves the boot path.

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
- **Addressing is DHCP with a hostname**, and the Mac asks for the machine by
  that name, never by an address it read under Ubuntu. Wi-Fi is out — the AX210
  needs a firmware image.
- **QEMU's `virtio-net-pci-non-transitional` on `q35` advertises no PCIe
  function-level reset** — measured, not assumed: `pcidev`'s refusal on that
  ground reddened every netd registration at once. So a re-claim is made safe by
  ordering instead: bus mastering starts on the claim's **first grant**, never at
  hand-over, so a function still holding its last holder's queue addresses can
  act on none of them. `release` asks for a reset where the function advertises
  one; the I219 does, so on the T14 both hold.
- **The machine serves its own log, and the Mac finds it by name.** `logd`
  serves the boot from its first line on TCP `41337` (`toyos-logstream`'s
  `PORT`), and netd answers multicast DNS for `toyos-t14.local` once it holds a
  lease (`toyos-mdns`), so nothing in the image names the Mac and nothing on the
  Mac listens. A boot that dies before netd leases still needs the stick.
- The metal loop is `toyos-metal` (`src/metal.rs`), and the T14 is run by the
  orchestrator alone.
