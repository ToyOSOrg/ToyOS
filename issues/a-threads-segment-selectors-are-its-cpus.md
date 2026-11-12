---
status: open
kind: finding
opened: 2026-11-11
---

# A thread's segment selectors are its CPU's

Ring 3 loads DS, ES, FS and GS with any selector CPL 3 may load (SDM Vol. 2B,
MOV): null or the user data or code descriptor, at any RPL. The x86-64 switch
(`kernel/src/arch/x86_64/hw.rs`) carries a thread's FS base and nothing of
those four selectors, so a thread switched in reads, with `mov ax, gs`, the
selector the thread before it on that CPU loaded, and loses its own. The
bases are not involved: every descriptor Ring 3 may load has base 0, the FS
base moves with the thread, and the kernel's GS base is swapped in at every
entry (`kernel/src/arch/x86_64/entry.rs`). Found by reading while building
that entry.

**Exit**: a thread reads back the DS, ES, FS and GS selectors it loaded after
its CPU has run a thread that loaded others, by the switch saving and
restoring them; GS's is loaded while the user's base is the live one, and
each base is written after its selector.
