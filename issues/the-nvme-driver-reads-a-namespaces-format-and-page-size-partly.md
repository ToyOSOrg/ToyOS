---
status: open
kind: defect
opened: 2026-10-23
---

# The NVMe driver reads a namespace's format and the page size partly

Every image drives NVMe by class code, so `Controller::open`
(`system/nvme/src/nvme.rs`) meets whatever controller a machine carries. Three
of its geometry reads assume what QEMU's and the benches' controllers report.
The field layouts come from NVM Express Base Specification 2.0: the Controller
Capabilities register, the Identify Controller data structure, and the Identify
Namespace data structure with its LBA Format data structure.

- **The LBA format index.** The driver takes `FLBAS` bits 3:0 as the index.
  When `NLBAF` is greater than 16, bits 6:5 hold the index's upper two bits, so
  on such a namespace the driver may read another format's `LBADS`.
- **Metadata.** The driver ignores the format's metadata size (`MS`, bits 15:0
  of the LBA format). With `FLBAS` bit 4 set (extended LBA), metadata is
  interleaved with the data, so one sector moves more than `2^LBADS` bytes
  through the PRPs. A separate-buffer format needs an `MPTR` the driver never
  sets.
- **Memory page size.** `CC.MPS` is written as 0 (4 KiB) without checking
  `CAP.MPSMIN`. `MDTS` is converted to blocks as though its unit were 4 KiB,
  but its unit is `2^(12 + MPSMIN)` bytes.

By reading, none of these crosses a partition. GPT's CRCs reject a misread
table, sessions are bounded to their partition's span, and DMA stays inside
the claim's domain. They are correctness gaps on unusual drives. They have not
been seen on any machine.

**Exit**: `Controller::open` refuses each unsupported case by name with
`Refusal::Geometry`, or supports it. Specifically:

- it takes the full format index;
- it refuses any format with `MS` non-zero;
- it refuses a controller whose `MPSMIN` is not 0, or sizes `MDTS` in that unit;
- a host model of the Identify and CAP fields holds each case.

**Owner**: the orchestrator, which dispatches it.
