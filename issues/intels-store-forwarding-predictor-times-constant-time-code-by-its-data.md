---
status: open
kind: defect
opened: 2026-11-01
---

# Intel's store-forwarding predictor times constant-time code by its data

On the T14 (Tiger Lake, signature 0x806c1, microcode 0xbe), toyos-crypto's
P-256 and P-384 public key and agreement at 9217793d2, whose branches and
addresses do not depend on the scalar, leaked through timing: max |t| 69 to
81 over 1,000,000 fixed-versus-random measurements, the fixed scalar 1 about
90 ticks slower in 316,000 for P-256. Turning on PSFD alone (`IA32_SPEC_CTRL`
bit 7), which disables only the Fast Store Forwarding Predictor, removed it,
and so did DOITM and SSBD. The T14 enumerates no `DDP_CTRL`, so it has no
data-dependent prefetcher.

- Evidence: the off/on pair,
  https://github.com/ToyOSOrg/evidence/tree/7fb806b73ce0397b9cbec9e1da1df910ba8f4785/pr-1038/r1/probe,
  and the attribution boot by mode,
  https://github.com/ToyOSOrg/evidence/tree/56a9678bcec3939074783b0f0a363eb14c1a25da/combined/fsfp-attribution.
- Intel lists FSFP on Ice Lake U/Y and SP, Lakefield, Tiger Lake U/H, Rocket
  Lake and Alder Lake P/M/S, and says later parts declare it by enumerating
  PSFD (CPUID.(7,2).EDX[0]), as the T14 does too (EDX 0x1). It does not
  document what makes the predictor forward wrongly, so no coding rule
  avoids it.
- Current code passes on the T14 with every such control off. That rests on
  avoiding the value pattern that triggered it (#1013 keeps the running point
  off the identity), which a test of chosen secrets cannot extend to every
  secret.
- By the owner's ruling (2026-10-26) ToyOS switches no DOITM on x86-64:
  constant time there rests on the code, as it must on AMD, which has no
  such mode. `toyos::timing` reports x86-64 unprotected.

**Owner**: the crypto track. **Exit**: a program holding secrets on a CPU
with FSFP runs with it disabled, by PSFD or SSBD through the per-program flag
of `issues/no-program-runs-with-speculative-store-bypass-disabled.md`, or
Intel documents which data the predictor depends on and toyos-crypto is shown
to keep secrets out of it. **Oracle**: `crypto_secret_timing_curves` on the
T14 at a revision that leaked, with the control on and off.
