---
status: open
kind: defect
opened: 2026-09-25
---

# A residue slot is held for the boot by a function never claimed again

`pcidev::release` keeps a function it could not reset in its slot as
`Slot::Residue` (`toyos-pci/src/slot.rs`), with the device addresses its last
holder's grants were at, so only that function's next claim is given the slot
and its grants are placed at those addresses. Nothing ends that hold except
that function's next claim. A function released without a reset and never
claimed again keeps one of `MAX_FUNCTIONS` slots for the rest of the boot:
no memory (the pages go back at the release), but a slot and a domain.

A boot that swaps out a reset-less function's service and never swaps it back,
or a manifest that stops naming the function, gets there: with one such
residue and three held slots, a claim of a fourth function is refused
`Exhausted` while a slot drives nothing.

Owner: the author of the residue (PR #492). Exit condition: a residue is given
up after a named, logged condition — a claim finding no free slot takes the
oldest residue slot and logs whose addresses it dropped, with the dropped
function detached from that domain first — and a guest test that leaves a
residue and then claims `MAX_FUNCTIONS` other functions is handed all of them.

A second, narrower gap in the same neighbourhood (`kernel/src/pcidev/mod.rs`,
`tear_down`'s `None` arm): a successor takes `RESIDUE[slot]` into
`Bound::residue` (`take_residue`) and places a grant at a range it chooses to
use, but is never required to place one at every range. A range it never
places is still mapped — `map_at` is what unmaps a residue range's old leaf,
by placing a fresh one over it — and when *that* successor is torn down in
turn, only `bound.grants` is unmapped and returned to `RESIDUE[slot]`;
`bound.residue`'s untouched leftover is simply dropped with the rest of
`Bound`. The range stays mapped, in nobody's `RESIDUE` and nobody's `grants`,
until a third holder's mastering reaches whatever the first holder's
descriptors named there and takes the fault the design promises the first
holder's own dead reach, one generation late — the same shape `T14` run 132
first showed.
