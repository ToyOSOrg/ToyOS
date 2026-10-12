---
status: open
kind: tooling
opened: 2026-10-11
---

# AMD erratum 1386's scope rests on Linux where AMD publishes no revision guide

`toyos_cpuvuln::erratum_1386` (`kernel/cpuvuln/src/erratum.rs`) refuses
`XSAVES` to the parts AMD's public revision guides list erratum 1386 against:
family 17h ZP-B2 (55449 rev 1.21), Rome-B0 (56323 rev 1.01), and family 19h
Milan-B1 and -B2 (56683 rev 1.08). AMD publishes no guide for family 17h
models 10h-2Fh, 40h-7Fh, 90h-91h or A0h-AFh, the client parts, the AMD
bench's model 60h among them; there the scope is the models Linux files as
Zen1 or Zen2 (`arch/x86/kernel/cpu/amd.c:482-492` at upstream `87db7795`),
a secondary source.

Evidence, each guide fetched from the Internet Archive's copy of
`www.amd.com/content/dam/amd/en/documents/processor-tech-docs/revision-guides/`
(`www.amd.com` itself answered neither HTTP/2 nor HTTP/1.1 from the
development host):
- the four guides above list the revisions named, and each says "No fix
  planned"; 57095 rev 1.01 (family 19h models 10h-1Fh) does not list 1386;
- the guides archived beside them, 58251 and 58730, set their text in fonts a
  stream-level text extraction does not decode, and were not read;
- Linux departs from AMD in three places: it names every Zen1 model where
  55449 clears ZP-B1 and PiR-B2, it lifts the erratum on ZP-B2 from microcode
  `0x0800126e` and on Rome-B0 from `0x08301052` (`amd.c:881-903`), which no
  guide states, and it does not name Milan. The kernel follows the guides on
  all three.

Owner: the orchestrator.

**Exit**: AMD's own statement of erratum 1386 for every family 17h model the
table names from Linux, and for 58251's and 58730's parts, read and the table
corrected to it; or the table cut to the guides' revisions alone, on the
owner's ruling that a part no guide names keeps `XSAVES`.
