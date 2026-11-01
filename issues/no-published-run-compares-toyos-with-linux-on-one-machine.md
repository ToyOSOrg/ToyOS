---
status: open
kind: tooling
opened: 2026-09-29
---

# No published run compares ToyOS with Linux on one machine

`tests/guest/osbench` is the one program, built for `x86_64-unknown-toyos`
and `x86_64-unknown-linux-musl`, that measures each bench under its Ubuntu
24.04 and under the shipped ToyOS kernel. The rows this issue asks for are
its groups:

- memcpy from 64 B to 64 MiB: `mem.memcpy.*`;
- pipe round trips, and throughput at 64 B to 64 KiB: `pingpong.pipe.*`,
  `pipe.throughput.*`;
- munmaps per second of a page a sibling on another core touched:
  `mem.munmap_touched_remote.4k`. Each side reads its APIC ID from
  CPUID.0BH:EDX (01H:EBX where 0BH enumerates nothing) before and after each
  window, and `osbench_rules::pair::admit` refuses the window when the two
  sides' IDs match or either side's changed across it;
- lateness past a 1 ms timer: `time.timer_1ms.late`;
- NVMe reads, read-only: `nvme.read.*`;
- the package's power, idle and under defined loads, with its C-state
  residency where Intel documents it: `energy.*`, run only by name, a ToyOS
  boot of its own (`osbench_energy`) and as root under Linux;
- the machine's PCI, USB, NVMe and cpuidle identity: `identity`, with no
  serial number or network identifier;
- TCP connect, round trip and throughput between the two benches:
  `lan.wired.*`, `lan.wifi.*`, the bench under test against the
  `osbench-peer` the other bench's Linux runs, each way. ToyOS and Linux on
  one path are the comparison, and Linux's runs on it its ceiling and its
  noise; `osbench-compare` reads the runs, interleaved, four of each at least
  for its exact test to resolve a difference. `lan` and `internet` run only
  by name, each its own metal row, since they need another party;
- a short CPU crunch, one thread then all, that stops at TjMax less 15 C
  where the package's temperature can be read: `crunch.*`.

`tests/guest/osbench/README.md` is how a comparison is run: both halves from
one revision, interleaved runs, Linux as installed for the baseline, and
Linux under ToyOS's frequency request as a labelled experiment. `persist`
runs on the bench's NVMe under both: the `osbench` row refuses a boot whose
DATA is not the NVMe's TOYOS-DATA partition. The README says why the AMD
bench's Linux keeps time on the HPET.

`internet.*`, HTTPS to Cloudflare's speed test, is exploratory evidence of a
path neither system owns, run only when a pass names its row: no automated
test depends on the public internet.

The first paired session (`ToyOSOrg/evidence` branch `pr-1065`, `r3/`) put
ToyOS's T14 LAN download and 64 B round trip at Linux's and its upload at
a third of it. That gap was closed by #1069, #1102, #1112 and #1114, and
the T14 parity session of #1147 read the upload and its first 300 ms at
Linux's ([evidence](https://github.com/ToyOSOrg/evidence/tree/1c9f4c83c362a1b30746f1e096ad412a2d5f25f9/r2/session)).

The cryptographic rows are not osbench's. AES-GCM, ChaCha20-Poly1305,
SHA-256 and the rest were measured under Linux on both benches against ring,
OpenSSL and AWS-LC by the crypto oracle probe, and that table is the
evidence: `ToyOSOrg/evidence` at `bed0b00efac82546b995f0841da54aea11c45694`,
`r2/table.md`.

The AMD bench's `crunch.single.deflate` read 4 to 6 % slower in four boots
of twelve, at the same clock, with and without the deep idle states
(`issues/the-amd-benchs-deflate-runs-6-percent-slow-in-some-boots-at-the-same-clock.md`).
An AMD single-thread row compared across boots needs repeats enough to
see which mode each boot read.

What ToyOS cannot measure is reported `unsupported` by name, and is not a
comparison until it can:

- **NVMe reads**: a program reaches a disk's blocks only through a grant the
  supervisor mints for a file server's role or the idle slot, and the bench's
  NVMe holds another system.
- **Package temperature**: no counter carries it
  (`toyos_abi::counters::Counter`), so the ToyOS crunch runs its one-thread
  phase alone and reports every all-core row `unmeasured`, with nothing to
  stop it at. The sampler's `IA32_PACKAGE_THERM_STATUS` and
  `MSR_TEMPERATURE_TARGET` in
  `issues/the-kernel-owns-cpu-performance-state.md` is what closes it; the
  package's energy is a counter now (`Counter::PackageEnergy`). The
  first ToyOS crunch on the T14 held 3,788 MHz all-core where Linux fell to
  1.7-1.9 GHz, which
  `issues/the-t14s-thermal-modes-do-not-move-its-package-power-limits.md`
  would explain: ToyOS refuses the firmware's PL1 and PL2 writes.
- **Wi-Fi**: `issues/there-is-no-wifi.md`. The T14's `lan.wifi.*` and every
  AMD bench `lan.*` and `internet.*` row, whose one network card is its
  AX200, are measured under Linux alone.
- **USB, NVMe and cpuidle identity**: their owners serve no program that
  reads them.
- **Loopback** (`net.*`): netstack refuses 127/8 as a destination
  (`issues/toyos-has-no-loopback-so-osbench-net-measures-nothing.md`).

**Exit**: both systems' outputs on the T14 and the AMD bench published in
evidence, Linux's run before the T14's wipe. Linux's outputs are a goal
ToyOS's are compared against in evidence, never a fixture a test reads.
