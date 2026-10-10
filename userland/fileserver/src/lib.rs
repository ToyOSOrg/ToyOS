//! A file server's decisions: the blocks memory stands in for a partition
//! with ([`ram`]) — a partition's own are `diskserver::disk`'s — the one
//! cache every byte of its volume passes through ([`cache`]), the volumes it
//! can serve ([`data`] for the bcachefs DATA role, [`fat`] for FAT32's LOG and
//! BOOT, [`absent`] for a role with no volume this boot), and the resolver that
//! keeps every path inside the directory a connection was given ([`resolve`]),
//! and which requests a read-only one is refused ([`rights`]).
//!
//! **This is the page cache.** A block of the volume — a btree node, a FAT
//! sector, a file's data — is read into [`cache::Cache`] once and served from
//! there; a write lands there and reaches the disk at a sync, or when the
//! cache has more dirty blocks than it keeps. The kernel holds none of it.
//!
//! The service around it is `src/main.rs`: its role's port and the grant on
//! each connection, its clients and their windows, and the wire (`toyos::fs`).

pub mod absent;
pub mod cache;
pub mod data;
pub mod fat;
pub mod ram;
pub mod resolve;
pub mod rights;
pub mod volume;
pub mod writeback;
