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
- an idle minute's package energy: `energy.idle_60s.package`;
- the machine's PCI, USB, NVMe and cpuidle identity: `identity`, with no
  serial number or network identifier;
- the wired LAN's TCP round trip and throughput, and Wi-Fi's under Linux:
  `lan.wired.*`, `lan.wifi.*`, against `osbench-peer` on another machine;
- a short CPU crunch, one thread then all, that stops at TjMax less 15 C
  where the package's temperature can be read: `crunch.*`.

The cryptographic rows are not osbench's. AES-GCM, ChaCha20-Poly1305,
SHA-256 and the rest were measured under Linux on both benches against ring,
OpenSSL and AWS-LC by the crypto oracle probe, and that table is the
evidence: `ToyOSOrg/evidence` at `bed0b00efac82546b995f0841da54aea11c45694`,
`r2/table.md`.

What ToyOS cannot measure is reported `unsupported` by name, and is not a
comparison until it can:

- **NVMe reads**: a program reaches a disk's blocks only through a grant the
  supervisor mints for a file server's role or the idle slot, and the bench's
  NVMe holds another system.
- **Package energy and temperature**: no counter carries either
  (`toyos_abi::counters::Counter`), so the ToyOS crunch runs its phases
  without a stop and says so.
- **Wi-Fi**: `issues/there-is-no-wifi.md`.
- **USB, NVMe and cpuidle identity**: their owners serve no program that
  reads them.
- **Loopback** (`net.*`): netstack refuses 127/8 as a destination.

**Exit**: both systems' outputs on the T14 and the AMD bench published in
evidence, Linux's run before the T14's wipe. Linux's outputs are a goal
ToyOS's are compared against in evidence, never a fixture a test reads.
