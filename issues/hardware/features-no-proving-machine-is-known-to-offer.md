---
status: open
kind: track
opened: 2026-09-29
---

# Features no proving machine is known to offer

No stage of
`issues/kernel/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`
or `issues/kernel/toyos-uses-what-modern-hardware-offers-for-speed.md` takes
these, since neither the T14 nor a nightly EPYC KVM guest is known to
enumerate one. It is blocked on a proving machine that does; its exit is every
row a stage issue under the track it serves, or a `rejected` issue. Intel's
parts are named by the SDM 325462-093US, Vol. 1 Table 5-2 ("SDM"), or by the
ISE reference 319433-062, Table 1-2 ("ISE"), neither of which places any of
these in Tiger Lake. AMD's and Arm's are read in Linux at tag `v7.2`; the AMD
parts that carry each were not verified against AMD's documents, and the Arm
cores were not verified against Arm's. The Arm rows belong to
`issues/kernel/toyos-runs-on-arm64.md`'s CPU-state declaration.

| Feature | Hardware |
|---|---|
| FRED and LKGS | Panther Lake, Clearwater Forest, Diamond Rapids (ISE) |
| LASS | Sierra Forest, Atom P6900, Core Ultra 200H (SDM) |
| LAM | the parts LASS names (SDM) |
| PKS | Alder Lake, Sapphire Rapids, Sierra Forest (SDM) |
| User interrupts (UINTR) | Sapphire Rapids, Sierra Forest, Atom P6900, Core Ultra 200H (SDM) |
| UMONITOR, UMWAIT, TPAUSE | Tremont, Alder Lake, Sapphire Rapids (SDM) |
| Fast zero-length `rep movsb`, fast short `rep stosb` | Alder Lake, Sapphire Rapids (SDM) |
| AMX | Sapphire Rapids (SDM) |
| AVX10.1, AVX10.2 | AVX10.1: Granite Rapids, Nova Lake; AVX10.2: Diamond Rapids, Nova Lake (ISE) |
| APX | Diamond Rapids, Nova Lake (ISE) |
| Thread Director, HRESET | Alder Lake (SDM) |
| TDX | Emerald Rapids (ISE) |
| Total Storage Encryption | Panther Lake (ISE) |
| TME | 11th-generation Core lines that set CPUID.(7,0):ECX bit 13, which varies by line (datasheet 631121-012 §1.3); the T14 reads ECX 0x18c05fde, bit 13 clear |
| INVLPGB, TLBSYNC | AMD, CPUID 0x80000008:EBX bit 3 (`arch/x86/include/asm/cpufeatures.h:331,335`); whether a nightly EPYC guest sees it waits on its runner's CPUID capture |
| SEV-SNP | AMD, CPUID 0x8000001F:EAX bit 4 (`cpufeatures.h:448,453`) |
| Shadow stack on AMD | AMD; unverified: that AMD enumerates it through the bit Linux reads for Intel, CPUID.(7,0):ECX bit 7 (`cpufeatures.h:390,397`), and which parts set it; whether a nightly EPYC guest sees it waits on its runner's CPUID capture |
| MTE | Armv8.5 (`arch/arm64/Kconfig:2150-2170`) |
| Pointer authentication | Armv8.3 (`arch/arm64/Kconfig:1967-1974`) |
| BTI | Armv8.5 (`arch/arm64/Kconfig:2087-2093`) |
| GCS | Armv9.4 (`arch/arm64/Kconfig:2236-2254`) |
| POE | Armv8.9 (`arch/arm64/Kconfig:2198-2213`) |
| SME | Arm (`arch/arm64/Kconfig:2311-2319`) |
| NVMe host memory buffer | an NVMe drive whose `id-ctrl` reads HMPRE non-zero; the T14's has not been read, and blockd builds the buffer only once a proving machine's does |
