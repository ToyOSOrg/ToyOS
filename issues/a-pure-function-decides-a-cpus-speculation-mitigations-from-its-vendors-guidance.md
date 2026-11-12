---
status: open
kind: track
opened: 2026-09-29
---

# A pure function decides a CPU's speculation mitigations from its vendor's guidance

**Built**: `kernel::mitigation::decide` maps a CPU's vendor, signature,
microcode revision, enumeration words (CPUID 7.0 EBX and EDX, 7.2 EDX,
0x80000008 EBX, 0x80000021 EAX and ECX, `IA32_ARCH_CAPABILITIES` where
enumerated) and whether another online CPU shares its core to a verdict per
vulnerability: unaffected, or the mitigations Intel's or AMD's published
guidance prescribes and which of them the CPU does not enumerate. Each rule
cites its document. Intel's affected-processor table is in it as its
machine-readable list gave it on 2026-09-04. Where a vendor prescribes a
protection for every program, the verdict follows ToyOS's policy, which sets
it for a program that asks: SRSO's IBPB between programs is marked as
policy, and Branch Type Confusion's STIBP for Jmp2Ret, which protects the
kernel from the core's other thread, is named as an SMT exposure. Each CPU reads its words at bring-up; once every CPU is
up the boot logs cpu0's words, the registers that show what is in force and a
line per vulnerability, and a later CPU's where they differ
(`arch/x86_64/mitigation.rs`). Each CPU sets AMD's LFENCE
dispatch-serializing bit (70586 G-2, MSR C001_1029[1]) where the decision
names it, after its microcode loads and before the clocksource's
`RDTSCP; LFENCE` relies on it, and stops the machine if the bit does not
hold. Both benches' words, as each one's boot logged them, are pinned in the
harness, and `checks::the_benches_words_decide_as_their_vendors_rows` holds
what they decide to each vendor's row for that CPU. Applying every other
mitigation the decision names is its own record's.

**Left**:
- Under the policy, an AMD CPU affected by Branch Type Confusion leaves the
  kernel's returns exposed to the core's other thread while it runs a
  program that has not asked, and always where it has no STIBP (Family 15h,
  Zen, Zen+). The lines say SMT vulnerable; the Trust page states it.
- Shared Buffers Data Read lets a program with MMIO access read the stale
  data other cores' MMIO left in the shared uncore buffers, and the decision
  names only the microcode's overwrite of what RDRAND, RDSEED and EGETKEY
  leave there. Intel's "Processor MMIO Stale Data Vulnerabilities" spares
  parts that enumerate `FBSDP_NO`, `MDS_NO` and `TAA_NO` its "Preventing
  Untrusted Software from Using Processor MMIO Stale Data Vulnerabilities to
  Extract Data", since nothing can read their fill buffers. The T14
  enumerates no `TAA_NO`, and this read takes the uncore's transaction
  buffers rather than fill buffers, so the item stays: Intel's remaining
  answer is to keep MMIO from untrusted software, and ToyOS's drivers hold
  MMIO.
- An AMD guest whose CPUID does not say LFENCE always serializes cannot set
  the bit and reports it lacking; the kernel's ordered TSC read is then not
  dispatch-ordered there. No proving machine is a guest.
- A core's two threads: Intel's "Intel Analysis of Microarchitectural Data
  Sampling" prescribes more than VERW where an attacker shares a core, and
  the decision says nothing of it. No proving machine is affected by MDS.
- An AMD CPU that enumerates neither `SSBD` nor `SsbdVirtSpecCtrl` is reported
  as lacking the control. AMD's "Speculative Store Bypass Disable" white paper
  gives Families 15h to 17h a model-specific register for it; the paper is not
  in hand, so no rule reads it.
- Intel's page lists CPUID 50657, B06F6 and B06F7, which its machine-readable
  list of the same date omits; the decision treats them as unlisted.
- AMD-SB-7008 (CVE-2023-20593, "Zen 2") is outside the decision: nothing holds
  a Zen 2 CPU's patch level to the bulletin's, or sets the register it names
  where the level is older.

**Exit**:
- A host test holds the function's verdict, for each proving machine's words,
  to the vendor's published row for that CPU, committed with its provenance.
- Each proving machine's boot logs the words ToyOS read there.
- **Mutation**: a vendor row dropped reds that machine's test.
- **Oracle**: the vendor's table, and ToyOS's own read on the machine.
