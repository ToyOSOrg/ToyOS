---
status: open
kind: finding
opened: 2026-09-02
---

# QEMU delivers a compatibility-format interrupt that the specification blocks

VT-d Rev. 4.0 (D51397-015) Section 5.1.4: when interrupt remapping is enabled,
"If Extended Interrupt Mode is enabled (EIME field in Interrupt Remapping Table
Address Register is Set), or if the Compatibility format interrupts are disabled
(CFIS field in the Global Status Register is Clear), the Compatibility format
interrupts are blocked."

QEMU 11.1.0 `hw/i386/intel_iommu.c:4376` does not do that:

```
    /* This is compatible mode. */
    if (addr.addr.int_mode != VTD_IR_INT_FORMAT_REMAP) {
        memcpy(translated, origin, sizeof(*origin));
        goto out;
    }
```

The message is copied through unchanged. `CFI` appears nowhere in that file, and
the three uses of `intr_eime` are the `IRTA_REG` decode, the migration state and
the destination-width extraction — none of them this gate. The kernel sets
`IRE`, never writes `GCMD.CFI`, and reads back `GSTS.CFIS` clear
(`gsts=0xc7000000`, bit 23 = 0), so the guest is asking for the blocking and the
model is not providing it.

The specification is the one to believe: it is the hardware contract, and the
blocking is the whole security property — without it a device that can write an
`0xFEEx_xxxx` DWORD injects any vector at any CPU, which is what interrupt
remapping exists to stop.

Measured, both arms, on the branch that turned interrupt remapping on. One
source at a time was left in compatibility format with `IRE` set and `CFIS`
clear, and the device went on working:

- the i8042's keyboard pin (GSI 1) left as a compatibility redirection entry:
  `PASS  i8042_keyboard  (6s)` — typed input still arrived.
- virtio-net's MSI-X (vector `0x22`) left at the compatibility address:
  `PASS  netd_hostile_peer  (4s)`, with `netd hostile peer: 6 malformed frames
  refused, 32 of 48 unidentified connections held, silent one dropped after
  2004 ms, netd alive` — real traffic still moved.

Consequences, which is why this is filed rather than noted:

- **A source nobody moved to the remappable format is invisible to every
  behavioural test in this suite.** It works on QEMU and black-screens on real
  hardware. `iommu_interrupt_remapping` is the only instrument in reach that can
  see one, and it sees it by reading the redirection entry and the MSI address
  back off the chip; it reds on both mutations above, naming the source.
- **The harness cannot certify the protection.** That a rogue device cannot
  inject a vector is unmeasurable here for the same reason. It is answerable on
  the T14 and nowhere else.

Exit condition: a same-session A/B on the T14 that shows a compatibility-format
message blocked under `CFIS=0`, or an upstream QEMU that implements the check.
Neither is owed by this project; what is owed is that no gate here is ever
written as though the harness could decide it.
