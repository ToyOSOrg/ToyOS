//! Every word an AMD-Vi unit and its driver exchange, as AMD 48882 rev 3.11
//! lays it out: the device table entry ([`dte`]), the host page tables a
//! translating entry names ([`table`]), the interrupt remapping table entry
//! ([`irte`]), the commands and event records ([`queue`]), and the register
//! fields this driver writes or reads ([`reg`]).
//!
//! The crate encodes one configuration: a requester is blocked, or translated
//! through a four-level host table that maps 2 MiB pages, read and write,
//! forced coherent; and its fixed and arbitrated messages are aborted, or
//! remapped through a table of its own indexed by vector. No entry passes DMA
//! untranslated or a message unremapped.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

pub mod dte;
pub mod irte;
pub mod queue;
pub mod reg;
pub mod table;
