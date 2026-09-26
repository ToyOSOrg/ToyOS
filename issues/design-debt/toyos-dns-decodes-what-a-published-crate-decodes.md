---
status: open
kind: design-debt
opened: 2026-09-26
---

# toyos-dns decodes DNS wire format that a published crate already decodes

Resolving names is the system's job and belongs in netd; decoding DNS
messages is not ToyOS's job. `toyos-dns` carries its own reader for names,
compression pointers and records (#511), where `hickory-proto` is the
widely used published decoder. The own reader was kept for size and a
bounded, fully tested parser at a trust boundary; the owner's rule is that
every line is a responsibility, and root `CLAUDE.md` says a crate that does
a general job is used, not written.

**Exit**: measure `hickory-proto`'s cost (dependencies, binary size, whether
it builds for ToyOS unchanged) against `toyos-dns`'s reader, and either
replace the reader with it — keeping `toyos-dns`'s decisions (which reply
answers a query, when to ask again) — or record the measured reason it stays.
