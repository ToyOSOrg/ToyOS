---
status: assigned
kind: track
opened: 2026-09-07
---

# The T14 answers only through a USB stick

Every boot of the bench is flashed to a stick under Ubuntu and judged by what
is read off it after a reboot into Ubuntu. The laptop is on a cable on the same
LAN as the development Mac and its NIC is the onboard Intel I219 at `00:1f.6`,
`8086:15fc`. The track is to make that cable the answer path. The boot on
which the Mac read the log, ran a command and handed the machine back over it
is deleted: the host reaches no T14 that runs ToyOS
(`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`).

What is left is the harness running userland tests over ssh through a russh
client, which `issues/the-t14-reboots-through-ubuntu-for-every-test.md`
stages, and a netboot spike in which the firmware fetches the loader over HTTP
so the stick leaves the boot path.

Constraints a reader would otherwise pay to re-derive:

- **The driver does not live in the kernel** (owner, 2026-09-07). The kernel owns
  the claim of the PCI function; the holder drives the registers, takes the
  interrupts and owns its DMA, which lives in that function's own IOMMU domain.
- **A machine with no IOMMU unit** is governed by
  `issues/a-machine-without-an-iommu-refuses-every-claim.md`. Today
  every claim there is refused by name, netd exits, and the machine boots on;
  `iommu_virtio_platform`'s no-unit arm is where that is read back. The T14
  has VT-d, so this is not a bound on the bench — but a `pcidev` refusal there
  is the first thing to check before suspecting the driver.
- **ssh is the bench's transport and a real feature**: sshd is built on russh
  and the harness's client was russh too, until it was deleted
  (`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`). No host
  ssh binary, no fork.
- **Addressing is DHCP with a hostname**, resolved through the router's DNS. The
  T14's MAC is the same under ToyOS and Ubuntu. Wi-Fi is out — the AX210 needs
  a firmware image.
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
