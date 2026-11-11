---
status: open
kind: track
opened: 2026-09-29
---

# A pure function decides a CPU's speculation mitigations from its vendor's guidance

**Built**: `kernel::mitigation::decide` maps a CPU's vendor, signature,
microcode revision and enumeration words (CPUID 7.0 EBX and EDX, 7.2 EDX,
0x80000008 EBX, 0x80000021 EAX and ECX, `IA32_ARCH_CAPABILITIES` where
enumerated) to a verdict per vulnerability: unaffected, or the mitigations
Intel's or AMD's published guidance prescribes and which of them the CPU does
not enumerate. Each rule cites its document. Intel's affected-processor table
is in it as its machine-readable list gave it on 2026-09-04. Each boot logs the
words the first CPU read and a line per vulnerability
(`arch/x86_64/mitigation.rs`), and a later CPU's where they differ. Each CPU
sets AMD's LFENCE dispatch-serializing bit (70586 G-2, MSR C001_1029[1]) where
the decision names it, after its microcode loads and before the kernel's own
LFENCE uses; the line reads it back. No other mitigation the decision names is
applied: each is its own record's.

**Left**:
- An AMD guest whose CPUID does not say LFENCE always serializes cannot set
  the bit and reports it lacking; the kernel's ordered TSC read and x2APIC
  write fence are then not dispatch-ordered there. No proving machine is a
  guest.
- Each proving machine's words, read from its own boot, as the words of the
  host test's row for that CPU. The rows there now carry the words the
  vendor's columns imply.
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
  to the vendor's published row for that CPU.
- Each proving machine's boot logs the words ToyOS read there.
- **Mutation**: a vendor row dropped reds that machine's test.
- **Oracle**: the vendor's table, and ToyOS's own read on the machine.
