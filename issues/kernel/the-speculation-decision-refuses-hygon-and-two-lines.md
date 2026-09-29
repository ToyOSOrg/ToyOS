---
status: open
kind: defect
opened: 2026-09-29
---

# The speculation decision refuses Hygon and two lines

`toyos_cpuvuln::decide` answers only from its facts, and at
`Ubuntu-6.8.0-142.142` these inputs are not decided:

- **Hygon**, refused whole: `bsp_init_hygon` sets `LS_CFG_SSBD` from its own
  `MSR_AMD64_LS_CFG` probe (`hygon.c:228-239`), which `Facts` does not carry.
- **`l1tf` on an affected CPU**: the line reads the e820 map against
  `x86_cache_bits` (`bugs.c:2538-2583`) and `kvm_intel`'s state
  (`bugs.c:3074-3089`).
- **`itlb_multihit` on an affected CPU**: the line reads `IA32_FEAT_CTL` and
  `CR4.VMXE` (`bugs.c:3091-3102`).

No machine ToyOS is tested on reaches one: the T14 is Intel, the TCG model is
AMD family 0xF, and the nightly's KVM runners are EPYC 7763, 9V74 and 9V45.

**Exit**: each is decided from facts the kernel reads and held by a fixture
captured under the pinned Linux on such a CPU.
