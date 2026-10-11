//! ROOT in memory: every decision the loader and the kernel make about the
//! image, pure. The loader reads the partition its slot names in bounded
//! [`chunk`]s; the kernel keeps the extent it is handed only once [`handoff`]
//! finds it inside memory the loader allocated.
//!
//! No `alloc`, no firmware, no `unsafe`: the loader and the kernel supply the
//! device, the candidates and the map.

#![no_std]
#![forbid(unsafe_code)]

pub mod chunk;
pub mod handoff;
