//! Ethernet, ARP, IPv4, ICMPv4, IGMP, UDP and TCP, parsed in place and built: a parse refuses by the first rule its input breaks, a build refuses what it cannot represent.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The bytes break the protocol's own format: no correct sender produces them.
    Malformed,
    /// Well formed, but a feature this stack does not implement.
    Unsupported,
}

macro_rules! reasons {
    ($name:ident { $($variant:ident = $text:literal, $class:ident;)* }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name {
            $($variant,)*
        }

        impl $name {
            /// The counter this refusal increments.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Port(core::num::NonZeroU16);

impl Port {
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

// Each compile_fail block sits beside one that compiles, so a typo cannot pass it.
#[cfg(doctest)]
mod compile_fail {
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
