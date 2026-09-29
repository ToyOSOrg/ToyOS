---
status: open
kind: defect
opened: 2026-09-29
---

# The speculation decision refuses AMD from family 0x15, Hygon, and two lines

`toyos_cpuvuln::decide` answers only from its facts, and three inputs rest on
state they do not carry, at `Ubuntu-6.8.0-142.142`:

- **AMD family 0x15 and later, and Hygon**, refused whole. `amd.c` derives
  `LS_CFG_SSBD` (576-594), the Zen generations (600-640), `TSA_SQ_NO`,
  `TSA_L1_NO` and `VERW_CLEAR` from a microcode table (517-530) and
  `IBPB_BRTYPE`/`SBPB` from a `PRED_CMD` write probe (799-805); `hygon.c`
  derives `LS_CFG_SSBD` (228-238). Each feeds SSB, RETBLEED, SRSO or TSA.
- **`l1tf` on an affected CPU**: the line reads the e820 map against
  `x86_cache_bits` (`bugs.c:2538-2583`) and `kvm_intel`'s state
  (`bugs.c:3074-3090`).
- **`itlb_multihit` on an affected CPU**: the line reads `IA32_FEAT_CTL` and
  `CR4.VMXE` (`bugs.c:3091-3102`).

The T14 and the TCG model reach none of them. The KVM arms of
`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md` (S2,
S3, S5, S6) expect "S1 over the reported facts" on the nightly's `-cpu host`
runners, and on an AMD Zen runner `decide` has no answer.

**Exit**: each is decided from facts the kernel reads — for AMD, the family,
the microcode revision and the probe's result — and held by a fixture captured
under the pinned Linux on such a CPU.
