//! A file server's decisions: the blocks memory stands in for a partition
//! with ([`ram`]) — a partition's own is `diskserver::disk::Served` — the
//! cache of its volume's structures ([`cache`]), its files' pages
//! ([`pages`]), the volumes it
//! can serve ([`data`] for the bcachefs DATA role, [`root`] for the SYSTEM
//! role's ROOT in memory, [`fat`] for FAT32's LOG and BOOT, [`absent`] for a
//! role with no volume this boot), and the resolver that
//! keeps every path inside the directory a connection was given ([`resolve`]),
//! which requests a read-only one is refused ([`rights`]), what a client
//! may take of the server and name in a request ([`bounds`]), the open
//! files its connections share ([`descriptions`]), and a directory's listing
//! in batches ([`listing`]).
//!
//! **A file's bytes are in the kernel's paged objects** ([`pages`]), one per
//! file, which its holders read without asking this server; a volume's own
//! structures — a btree node, a FAT sector — are in [`cache::Cache`]. A write
//! lands in either and reaches the disk at a sync, or when either holds more
//! dirty than it keeps.
//!
//! The service around it is `src/main.rs`: its role's port and the grant on
//! each connection, its clients and their windows, and the wire (`toyos::fs`).

pub mod absent;
pub mod bounds;
pub mod cache;
pub mod data;
pub mod descriptions;
pub mod fat;
pub mod listing;
pub mod pages;
pub mod ram;
pub mod resolve;
pub mod rights;
pub mod root;
pub mod volume;
pub mod writeback;
