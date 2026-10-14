---
status: open
kind: track
opened: 2026-09-29
---

# A pure function decides a CPU's speculation mitigations from its vendor's guidance

Nothing decides which speculative-execution mitigations a CPU needs. The
function to build maps a CPU's signature, microcode revision and enumeration
words to the mitigations Intel's or AMD's published affected-processor guidance
prescribes for it. Its enumeration words are CPUID 7.0 EDX, 0x80000008 EBX,
0x80000021 EAX and `IA32_ARCH_CAPABILITIES` where enumerated. #915 deleted
`toyos-cpuvuln`, which reproduced Linux's decision with no caller.

**Exit**:
- A host test holds the function's verdict, for each proving machine's words,
  to the vendor's published row for that CPU, committed with its provenance.
- Each proving machine's boot logs the words ToyOS read there.
- **Mutation**: a vendor row dropped reds that machine's test.
- **Oracle**: the vendor's table, and ToyOS's own read on the machine.
