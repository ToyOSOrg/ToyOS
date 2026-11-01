//! A disk driver's side of the block service: the partitions its own table
//! names ([`table`]), its link to the service on the `disk` port, over which
//! it offers its disks and is handed the sessions opened on them ([`link`]),
//! and each disk's sessions, which it serves itself ([`sessions`]). The
//! session region, which both ends map, is [`region`].
//!
//! Every disk driver links this, whatever its controller: what differs is the
//! [`sessions::Device`] beneath.

pub mod content;
pub mod designate;
pub mod link;
pub mod region;
pub mod sessions;
pub mod table;

pub use sessions::{Device, Owner, Sessions};
pub use table::Partition;

/// A partition GUID as the lines say it.
pub fn guid_text(bytes: [u8; 16]) -> String {
    toyos_gpt::Guid(bytes).to_string()
}
