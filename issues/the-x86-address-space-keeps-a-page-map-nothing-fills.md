---
status: open
kind: defect
opened: 2026-09-28
---

# The x86-64 address space keeps a page map nothing fills

`kernel/src/arch/x86_64/paging.rs`'s `AddressSpace` carries
`pages: HashMap<u64, PhysPage>`, documented as the user pages it frees on
drop, and `unmap` removes from it; nothing anywhere inserts into it, so it is
always empty and the removal does nothing. Dead code the compiler cannot see,
because a field that is read is not dead to it.

**Exit condition**: the field and its removal are gone, or what it claims to
own is put in it by the path that maps the page.
