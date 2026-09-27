//! Parsing and serialising the frames ToyOS's network stack speaks, in place:
//! Ethernet II with 802.1Q/802.1ad tags, ARP, IPv4 with options, ICMPv4, IGMP,
//! UDP, TCP with its options, and the Internet checksum with its incremental
//! update.
//!
//! **The crate classifies; it decides nothing.** Every parse ends in exactly one
//! of an accepted view or a refusal whose variant names the rule that failed,
//! and the rules run in a fixed order, so an input with several defects is
//! refused for the first one and a counter keyed on [`Class`] and `name()` is
//! deterministic. Which accepted packets are *ours*, which are refused by
//! policy and what is answered belong to the IP and transport layers above.
//!
//! **Parse, don't validate.** A view exposes only checked data: every length a
//! header names is checked against the bytes present before it is used, an
//! option list is walked once at parse and iterating it cannot fail, and a
//! transport parse takes the [`ipv4::Ipv4Packet`] it arrived in rather than
//! bytes, so its checksum cannot be verified against another datagram's
//! addresses. Views borrow the input and copy nothing; they hold the exact
//! bytes they were parsed from, so every accepted packet re-emits byte for
//! byte, unknown options, reserved bits and all.
//!
//! **A builder writes every byte it owns** — padding, reserved and unused
//! fields are zero whatever the buffer held — and computes every length,
//! offset and checksum itself. What a builder cannot represent is refused with
//! a [`BuildError`], never truncated or clamped, and what its types can rule
//! out they do: a port is never zero, an interface MAC is never a group
//! address, a TTL is never zero, and an established TCP segment has no field
//! for a SYN-only option.
//!
//! Legacy behaviour is not implemented, so its input is refused by name
//! (`icmp.source-quench`, `eth.length-frame`, an illegal TCP option length);
//! the caller counts and logs each refusal.
//!
//! `no_std`, no allocation, no `unsafe`, no I/O: a parse never panics, never
//! reads outside its input and never allocates.

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

/// What kind of defect a refused input has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The bytes break the protocol's own format: no correct sender produces them.
    Malformed,
    /// Well formed, but a feature this stack does not implement.
    Unsupported,
}

/// One refusal enum per protocol: each variant is one rule and one counter.
macro_rules! reasons {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal, $class:ident;)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
        }

        impl $name {
            /// The counter this refusal increments.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

            /// Whether the input was malformed or merely unsupported.
            pub const fn class(self) -> $crate::Class {
                match self {
                    $(Self::$variant => $crate::Class::$class,)*
                }
            }
        }
    };
}

pub mod arp;
pub mod checksum;
mod emit;
pub mod ethernet;
pub mod icmp;
pub mod igmp;
pub mod ipv4;
pub mod tcp;
pub mod udp;

pub use emit::BuildError;

/// A port number that is not zero: port 0 is reserved and nothing listens on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Port(core::num::NonZeroU16);

impl Port {
    /// `None` for port 0.
    pub const fn new(port: u16) -> Option<Self> {
        match core::num::NonZeroU16::new(port) {
            Some(port) => Some(Self(port)),
            None => None,
        }
    }

    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// The compile-time scenarios: each block must fail to compile for the reason
/// its scenario names, beside a block that compiles, so a typo cannot pass one.
#[cfg(doctest)]
mod compile_fail {
    /// ETH-30: the builder's EtherType admits only IPv4 and ARP, so a frame
    /// with type 0x05DC cannot be expressed.
    ///
    /// ```
    /// use toyos_net_wire::ethernet::TxEtherType;
    /// let _ = TxEtherType::Ipv4;
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::ethernet::TxEtherType;
    /// let _ = TxEtherType::Other(0x05dc);
    /// ```
    #[allow(non_camel_case_types)]
    pub struct s_eth_030_builder_ethertype_admits_only_ipv4_and_arp;

    /// TSER-08: an established segment's options hold Timestamps and SACK
    /// blocks only; MSS, Window Scale and SACK-Permitted have no field.
    ///
    /// ```
    /// use toyos_net_wire::tcp::{EstablishedOptions, Timestamps};
    /// let _ = EstablishedOptions { timestamps: Some(Timestamps { value: 1, echo: 0 }), sack: &[] };
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::tcp::EstablishedOptions;
    /// let _ = EstablishedOptions { timestamps: None, sack: &[], mss: Some(1460) };
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::tcp::{EstablishedOptions, WindowShift};
    /// let _ = EstablishedOptions { timestamps: None, sack: &[], window_scale: WindowShift::new(7) };
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::tcp::EstablishedOptions;
    /// let _ = EstablishedOptions { timestamps: None, sack: &[], sack_permitted: true };
    /// ```
    #[allow(non_camel_case_types)]
    pub struct s_tser_008_established_builder_cannot_hold_syn_only_options;

    /// RT-10: a transport parse takes the parsed IPv4 view and nothing else,
    /// so its checksum is verified against the addresses its bytes came with.
    ///
    /// ```
    /// use toyos_net_wire::{ipv4::Ipv4Packet, udp::UdpDatagram};
    /// fn parse<'a>(ip: &Ipv4Packet<'a>) -> bool {
    ///     UdpDatagram::parse(ip).is_ok()
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::{ipv4::Ipv4Packet, udp::UdpDatagram};
    /// fn parse<'a>(ip: &Ipv4Packet<'a>, bytes: &'a [u8]) -> bool {
    ///     UdpDatagram::parse(bytes).is_ok()
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::{ipv4::Ipv4Packet, tcp::TcpSegment};
    /// fn parse<'a>(ip: &Ipv4Packet<'a>, bytes: &'a [u8]) -> bool {
    ///     TcpSegment::parse(bytes, ip).is_ok()
    /// }
    /// ```
    #[allow(non_camel_case_types)]
    pub struct s_rt_010_transport_parse_takes_only_its_ipv4_view;
}
