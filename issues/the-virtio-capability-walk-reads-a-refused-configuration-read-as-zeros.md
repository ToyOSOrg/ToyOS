---
status: open
kind: defect
opened: 2026-10-09
---

# The virtio capability walk reads a refused configuration read as zeros

`vendor_caps` (`userland/netstack/src/virtio_net.rs`) walks a function's
capability list through its claim, and two things in it believe more than they
were told:

- A field of a vendor capability is read as
  `dev.config_read(next + at, width).unwrap_or(0)`, so a read the kernel
  refused becomes a `cfg_type`, `bar`, `offset` or `length` of 0. A capability
  made of zeros is then refused or ignored by `toyos_virtio::pci::Layout::of`
  under some other name (`MissingCap`, `TooShort`), and the kernel's word for
  what happened is gone.
- The capabilities pointer and every `next` link are used as read. The *PCI
  Local Bus Specification*, revision 3.0, section 6.7, reserves the bottom two
  bits of each and has software mask them before using the value as an offset
  (cited from memory: the document was not at hand when this was filed).
  An unmasked pointer with either bit set is an offset no capability is at,
  and the 32-bit reads at `next + 8` and on are then misaligned, which the
  claim refuses and the first item turns into zeros.

Neither reaches memory: the claim bounds every read to the function's own
configuration space (`config_space_is_bounded`). What they cost is a device
refused for the wrong reason.

Owned by the second client of `toyos-virtio`, which moves the walk into the
crate (stage 1 of `issues/every-driver-is-still-in-the-kernel.md`). Exit: the
walk is the crate's, over a configuration read its caller passes in; a refused
read is a refusal by name and a link is masked before it is an offset; and a
host test is red against each of the two as they stand today.
