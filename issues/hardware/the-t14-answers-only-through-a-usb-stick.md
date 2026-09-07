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

**Stage 1 is done**: the substrate under which a process that holds a claim on a
PCI function drives it — a register window, DMA the unit translates for that
function alone, and the interrupt — proven by moving the virtio-net driver out
of the kernel into netd (`kernel/src/pcidev.rs`,
`userland/netd/src/virtio_net.rs`). What is left:

2. **The I219 driver in netd**, on that substrate: DHCP with the hostname
   `toyos-t14`, first ping and first ssh from the Mac.
3. **A record stream from logd to a listener in the harness**, so a boot's log
   arrives while it is booting.
4. **sshd grows command execution, file transfer both ways and key auth**, and
   the harness runs userland tests over ssh with a russh client.
5. **A netboot spike**: the firmware fetches the loader over HTTP, so the stick
   leaves the boot path.

Constraints a reader would otherwise pay to re-derive:

- **The driver does not live in the kernel** (owner, 2026-09-07). The kernel owns
  the claim of the PCI function; the holder drives the registers, takes the
  interrupts and owns its DMA, which lives in that function's own IOMMU domain.
- **ssh is the bench's transport and a real feature**: sshd is built on russh
  and the harness's client is russh too. No host ssh binary, no fork.
- **Addressing is DHCP with a hostname**, resolved through the router's DNS. The
  T14's MAC is the same under ToyOS and Ubuntu, so the lease is the one `t14`
  already resolves to. Wi-Fi is out — the AX210 needs a firmware image.
- The I219 has **32-bit BARs**, and `pcidev`'s window allocator has only ever
  placed a 64-bit one: `Refusal::NoWindow` on that machine is the first thing
  stage 2 will meet, and what it means is that nothing was found above what
  firmware assigned and below the platform's fixed MMIO.
- The metal loop is `toyos-metal` (`src/metal.rs`), and the T14 is run by the
  orchestrator alone.
