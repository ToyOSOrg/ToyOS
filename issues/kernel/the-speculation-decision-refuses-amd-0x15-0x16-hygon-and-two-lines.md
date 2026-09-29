---
status: open
kind: defect
opened: 2026-09-29
---

# The speculation decision refuses AMD 0x15, 0x16, Hygon, and two lines

`toyos_cpuvuln::decide` answers only from its facts, and at
`Ubuntu-6.8.0-142.142` these inputs are not decided:

- **AMD family 0x15, 0x16, 0x18 and from 0x1B, and Hygon**, refused whole.
  The pinned Linux names 0x15 and 0x16 for Retbleed (`common.c:1338-1339`),
  and `hygon.c` derives `LS_CFG_SSBD` from its own probe (228-239); no fixture
  holds a reading of any of them.
- **`l1tf` on an affected CPU**: the line reads the e820 map against
  `x86_cache_bits` (`bugs.c:2538-2583`) and `kvm_intel`'s state
  (`bugs.c:3074-3089`).
- **`itlb_multihit` on an affected CPU**: the line reads `IA32_FEAT_CTL` and
  `CR4.VMXE` (`bugs.c:3091-3102`).

No machine ToyOS is tested on reaches one: the T14 is Intel, the TCG model is
AMD family 0xF, and the nightly's KVM runners are EPYC 7763, 9V74 and 9V45.

**Exit**: each is decided from facts the kernel reads and held by a fixture
captured under the pinned Linux on such a CPU.
