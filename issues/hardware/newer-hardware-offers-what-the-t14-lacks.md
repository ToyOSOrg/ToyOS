---
status: open
kind: track
opened: 2026-09-29
---

# Newer hardware offers what the T14 lacks

Owner ruling: the T14 gets every security and speed feature its hardware has
(`issues/kernel/the-kernel-is-at-least-as-secure-as-linux-on-the-t14.md`,
`issues/kernel/toyos-uses-what-the-t14s-hardware-offers-for-speed.md`), and
what only other modern systems have is written down here. It is blocked on a
machine that has the feature; its exit is every row a stage on that
machine's track or a `rejected` issue. Intel's parts are named by the
SDM 325462-093US, Vol. 1 Table 5-2 ("SDM"), or by the ISE reference
319433-062, Table 1-2 ("ISE"), neither of which places any of these in Tiger
Lake. AMD's and Arm's are read in Linux at tag `v7.2`; the AMD parts that
carry each were not verified against AMD's documents, and the Arm cores were
not verified against Arm's. The Arm rows belong to
`issues/kernel/toyos-runs-on-arm64.md`'s CPU-state declaration.

| Feature | Hardware | Why it matters to ToyOS |
|---|---|---|
| FRED and LKGS | Panther Lake, Clearwater Forest, Diamond Rapids (ISE) | Replaces the IDT and SYSCALL entry; `SWAPGS`, `SETSSBSY` and `CLRSSBSY` are `#UD` under it (SDM Vol. 3A §8.5.1), so FSGSBASE (speed track P5) costs no `swapgs` and a kernel shadow stack (security S13) needs no tokens |
| LASS | Sierra Forest, Atom P6900, Core Ultra 200H (SDM) | Refuses a user access to the kernel half before any page walk whose timing would map the kernel's layout (SDM Vol. 3A §4.3), which security S10's KASLR rests on; Linux v7.2 sets it on every CPU (`arch/x86/kernel/cpu/common.c:413-431`) |
| LAM | the parts LASS names (SDM) | Metadata in a user pointer's unused bits, for sanitizers and JITs; Linux v7.2 holds it back until LASS (`arch/x86/Kconfig:2150-2153`) |
| PKS | Alder Lake, Sapphire Rapids, Sierra Forest (SDM) | Protection keys for kernel pages (SDM Vol. 3A §5.6.2): page tables and other kernel state writable only inside the code that owns them |
| User interrupts (UINTR) | Sapphire Rapids, Sierra Forest, Atom P6900, Core Ultra 200H (SDM) | `SENDUIPI` interrupts another thread at CPL 3 (SDM Vol. 3A §9.1): userland servers wake each other without a syscall |
| UMONITOR, UMWAIT, TPAUSE | Tremont, Alder Lake, Sapphire Rapids (SDM) | A userland driver or lock waits at low power without a syscall |
| Fast zero-length `rep movsb`, fast short `rep stosb` | Alder Lake, Sapphire Rapids (SDM) | Speed track P2's copies, and `memset`, cheaper at small sizes |
| AMX | Sapphire Rapids (SDM) | Matrix tiles for user programs; 8192 bytes of XSAVE state per thread (SDM Vol. 1 §13.1), so the kernel grants it per process |
| AVX10.1, AVX10.2 | AVX10.1: Granite Rapids, Nova Lake; AVX10.2: Diamond Rapids, Nova Lake (ISE) | AVX-512-class vectors on client parts again: Alder Lake has none (SDM Table 5-2, note 3) |
| APX | Diamond Rapids, Nova Lake (ISE) | Extended general-purpose registers, a new XSAVE component (ISE revision history, EGPR) the kernel saves, and a target feature for the toolchain |
| Thread Director, HRESET | Alder Lake (SDM) | The core-class hints `issues/kernel/all-cores-are-assumed-equal-and-arm64-breaks-that.md` needs on a hybrid part |
| TDX | Emerald Rapids (ISE) | ToyOS as a confidential guest; ToyOS runs no guest today |
| Total Storage Encryption | Panther Lake (ISE) | A platform engine encrypts storage under a key `PBNDKB` wraps to the platform (SDM Vol. 2B, PBNDKB), so disk encryption's key reaches memory only wrapped |
| TME | 11th-generation Core lines that set CPUID.(7,0):ECX bit 13, which varies by line (datasheet 631121-012 §1.3); the T14 reads ECX 0x18c05fde, bit 13 clear | Memory encrypted under a key firmware activates and locks; the kernel reads `IA32_TME_ACTIVATE` on every CPU and refuses a boot where two disagree, where Linux only reports it (`detect_tme_early`) |
| INVLPGB, TLBSYNC | AMD, CPUID 0x80000008:EBX bit 3 (`arch/x86/include/asm/cpufeatures.h:331,335`) | Broadcast TLB invalidation without IPIs, which Linux v7.2 uses (`arch/x86/mm/tlb.c:276-285,407-448`); it would replace speed track P4's shootdown IPIs, as `TLBI IS` does on Arm |
| SEV-SNP | AMD, CPUID 0x8000001F:EAX bit 4 (`cpufeatures.h:448,453`) | ToyOS as a confidential guest; ToyOS runs no guest today |
| Shadow stack on AMD | AMD; unverified: that AMD enumerates it through the bit Linux reads for Intel, CPUID.(7,0):ECX bit 7 (`cpufeatures.h:390,397`), and which parts set it | Security S11 and S13 would run there unchanged |
| MTE | Armv8.5 (`arch/arm64/Kconfig:2150-2170`) | Always-on detection of memory errors at runtime, in C programs and unsafe Rust |
| Pointer authentication | Armv8.3 (`arch/arm64/Kconfig:1967-1974`) | Signed return addresses and pointers: Arm's answer to ROP |
| BTI | Armv8.5 (`arch/arm64/Kconfig:2087-2093`) | Arm's IBT: security S12 on the arm64 port |
| GCS | Armv9.4 (`arch/arm64/Kconfig:2236-2254`) | Arm's shadow stack: security S11 and S13 on the arm64 port |
| POE | Armv8.9 (`arch/arm64/Kconfig:2198-2213`) | Arm's protection keys: security S14 on the arm64 port |
| SME | Arm (`arch/arm64/Kconfig:2311-2319`) | Matrix state for user programs, granted per process as AMX is |
