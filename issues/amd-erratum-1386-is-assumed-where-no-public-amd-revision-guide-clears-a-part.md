---
status: open
kind: tooling
opened: 2026-10-11
---

# AMD erratum 1386 is assumed where no public AMD revision guide clears a part

`kernel::xsave::erratum_1386` (`kernel/pure/xsave.rs`) refuses `XSAVES` to
the parts AMD's public revision guides list erratum 1386 against, "No fix
planned": ZP-B2 (55449 rev 1.21), Rome-B0 (56323 rev 1.01), Milan-B1 and -B2
(56683 rev 1.08). It also refuses it to every other AMD family 17h and 19h part,
and to a family 18h part of any vendor but Intel, unless a guide clears it.
The cleared parts are ZP-B1 and PiR-B2 (55449) and Genoa-B1 (57095 rev 1.01,
which does not list the erratum). The rule rests on the cost asymmetry, not on
a document. With `IA32_XSS` 0, `XSAVEC` saves what `XSAVES` would, and only
the modified optimisation is lost. A wrong no hands a thread an older XMM value
of its own (orchestrator's ruling A on #915).

Evidence, in [`amd-guides.md`](https://github.com/ToyOSOrg/evidence/blob/d4754efa08523eaddecf53dc8f9239b35e4b4268/design/amd-guides.md):
- every guide the Internet Archive holds under AMD's `processor-tech-docs/revision-guides/` prefix, hashed;
- 58251 and 58730, family 1Ah, are read too and do not list 1386;
- the guide for family 17h models 60h-6Fh, which PPR 55922 names as docRevG, is not public;
- `www.amd.com/system/files/TechDocs/` and `developer.amd.com/wp-content/resources/` were not searched.

Family 1Ah models outside 00h-1Fh are unguided and not named: the ruling covers
families 17h to 19h.

Owner: the orchestrator.

**Exit**: a separate reader's clean-room specification of AMD's own code for
erratum 1386 settles the models no public guide covers, and the rule is
corrected to it.
