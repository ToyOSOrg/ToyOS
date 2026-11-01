---
status: open
kind: defect
opened: 2026-11-01
---

# The AVX2 Poly1305 runs under Intel's MXCSR-dependent timing

Intel's "MXCSR Configuration Dependent Timing" guidance says that on a CPU
that does not enumerate `MCDT_NO` (CPUID.(07H,2):EDX[5]), some vector
instructions can retire up to one cycle later on particular data under an
MXCSR other than 0x1FBF. `VPMULUDQ` is on its list, and toyos-crypto's AVX2
Poly1305 kernel (`lib/crypto/src/arch/x86_64/poly1305.rs`) runs it on the
secret key's limbs under the default MXCSR (0x1F80). The T14 does not
enumerate `MCDT_NO`: its CPUID.(07H,2):EDX reads 0x1, in the boot at
https://github.com/ToyOSOrg/evidence/tree/56a9678bcec3939074783b0f0a363eb14c1a25da/combined/fsfp-attribution.
DOITM does not cover it, and by the owner's ruling (2026-10-26) ToyOS
switches no DOITM on x86-64 anyway.

Whether the delay is measurable on our benches is unmeasured.

**Owner**: the crypto track. **Exit**: where `MCDT_NO` is clear, the AVX2
Poly1305 kernel runs under Intel's sequence (`STMXCSR`; `LDMXCSR` 0x1FBF;
`LFENCE` before the kernel; `LFENCE`; `LDMXCSR` of the saved value after),
its cost measured on the T14. **Oracle**: Intel's instruction list and the
CPU's `MCDT_NO` bit.
