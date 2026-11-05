//! File-server volumes, namespace resolution and open-file ownership.
//! Persistent file contents use supplied paged objects; memory volumes keep
//! their bytes in anonymous objects until the last name and holder are gone.
//! The service and its capability protocol are in `main.rs`.

pub mod absent;
pub mod bounds;
pub mod cache;
pub mod data;
pub mod descriptions;
pub mod fat;
pub mod listing;
pub mod memory;
pub mod pages;
pub mod ram;
pub mod resolve;
pub mod rights;
pub mod root;
pub mod volume;
pub mod writeback;
