//! Ethernet, ARP, IPv4, ICMPv4, IGMP, UDP and TCP, parsed in place and built: a parse refuses by the first rule its input breaks, a build refuses what it cannot represent.
//!
//! It is also what every net crate above it shares: the caller's clock, the keyed function, and
//! the declaration of a crate's counters.

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
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

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

/// Declares a crate's counters: `Counter`, each variant the name inspect reads it by and, marked
/// `logged`, a refusal of legacy or insecure input the log names; `Counters`, one value each; and
/// `PerCounter<T>`, one `T` each.
#[macro_export]
macro_rules! counters {
    ($($variant:ident = $name:literal $(, $logged:ident)?;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Counter {
            $($variant,)*
        }

        impl Counter {
            pub const ALL: &'static [Counter] = &[$(Counter::$variant,)*];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            pub const fn logged(self) -> bool {
                match self {
                    $(Self::$variant => $crate::counters!(@logged $($logged)?),)*
                }
            }
        }

        #[allow(non_snake_case)]
        #[derive(Clone, Debug, Default)]
        struct PerCounter<T> {
            $($variant: T,)*
        }

        impl<T> PerCounter<T> {
            fn get(&self, counter: Counter) -> &T {
                match counter {
                    $(Counter::$variant => &self.$variant,)*
                }
            }

            fn get_mut(&mut self, counter: Counter) -> &mut T {
                match counter {
                    $(Counter::$variant => &mut self.$variant,)*
                }
            }
        }

        #[derive(Clone, Debug, Default)]
        pub struct Counters(PerCounter<u64>);

        impl Counters {
            pub fn get(&self, counter: Counter) -> u64 {
                *self.0.get(counter)
            }

            pub(crate) fn add(&mut self, counter: Counter, n: u64) {
                let value = self.0.get_mut(counter);
                *value = value.saturating_add(n);
            }

            pub fn iter(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
                Counter::ALL.iter().map(|&c| (c.name(), self.get(c)))
            }
        }
    };
    (@logged logged) => {
        true
    };
    (@logged) => {
        false
    };
}

pub mod arp;
pub mod checksum;
mod emit;
pub mod ethernet;
pub mod icmp;
pub mod igmp;
pub mod ipv4;
pub mod siphash;
pub mod tcp;
pub mod udp;

pub use emit::BuildError;

/// A point on the caller's monotonic clock, in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    pub const fn from_nanos(ns: u64) -> Self {
        Self(ns)
    }

    pub const fn from_millis(ms: u64) -> Self {
        Self(ms.saturating_mul(1_000_000))
    }

    pub const fn nanos(self) -> u64 {
        self.0
    }

    pub fn after(self, d: core::time::Duration) -> Self {
        Self(self.0.saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)))
    }

    /// Zero when `earlier` is not earlier.
    pub const fn since(self, earlier: Self) -> core::time::Duration {
        core::time::Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

    /// ```
    /// use toyos_net_wire::ipv4::{Protocol, RawPayload};
    /// let Protocol::Other(protocol) = Protocol::from_number(253) else { panic!() };
    /// let _ = RawPayload { protocol, bytes: &[] };
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::ipv4::{Protocol, RawPayload};
    /// let _ = RawPayload { protocol: Protocol::Udp, bytes: &[] };
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::{checksum::PseudoHeader, ipv4::{Protocol, WritePayload}, BuildError};
    /// struct Forged;
    /// impl WritePayload for Forged {
    ///     fn protocol(&self) -> Protocol {
    ///         Protocol::Udp
    ///     }
    ///     fn length(&self, _header_len: usize) -> Result<usize, BuildError> {
    ///         Ok(0)
    ///     }
    ///     fn write(&self, _pseudo: &PseudoHeader, _out: &mut [u8]) -> Result<(), BuildError> {
    ///         Ok(())
    ///     }
    /// }
    /// ```
    #[allow(non_camel_case_types)]
    pub struct raw_payload_takes_no_transport_protocol_and_no_payload_is_forged;

    /// ```
    /// use toyos_net_wire::checksum::PseudoHeader;
    /// use toyos_net_wire::ethernet::{FrameBody, FrameBuilder, IndividualMac, MacAddr};
    /// use toyos_net_wire::ipv4::{Ipv4Builder, Ipv4Source, Payload, Protocol, TrafficClass, Ttl};
    /// use toyos_net_wire::udp::UdpBuilder;
    /// use toyos_net_wire::Port;
    /// fn frame<B: FrameBody>(body: &B, out: &mut [u8]) -> usize {
    ///     let source = IndividualMac::new(MacAddr([0x02, 0, 0, 0, 0, 1])).unwrap();
    ///     FrameBuilder { destination: MacAddr::BROADCAST, source }.emit(body, out).unwrap().len()
    /// }
    /// fn send<P: Payload>(payload: P, out: &mut [u8]) -> usize {
    ///     let builder = Ipv4Builder {
    ///         source: Ipv4Source::new(std::net::Ipv4Addr::new(192, 0, 2, 1)).unwrap(),
    ///         destination: std::net::Ipv4Addr::new(192, 0, 2, 2),
    ///         ttl: Ttl::DEFAULT,
    ///         traffic_class: TrafficClass::ZERO,
    ///         options: &[],
    ///         payload,
    ///     };
    ///     frame(&builder, out)
    /// }
    /// let udp = UdpBuilder { source: Port::new(1).unwrap(), destination: Port::new(2).unwrap(), data: b"hi" };
    /// assert_eq!(send(udp, &mut [0xAA; 100]), 60);
    /// let pseudo = PseudoHeader {
    ///     source: std::net::Ipv4Addr::new(192, 0, 2, 1),
    ///     destination: std::net::Ipv4Addr::new(192, 0, 2, 2),
    ///     protocol: Protocol::Tcp,
    ///     length: 100,
    /// };
    /// let _ = pseudo.accumulator().sum();
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::ethernet::{TxEtherType, WriteFrameBody};
    /// use toyos_net_wire::BuildError;
    /// struct Forged;
    /// impl WriteFrameBody for Forged {
    ///     const ETHER_TYPE: TxEtherType = TxEtherType::Ipv4;
    ///     fn length(&self) -> Result<usize, BuildError> {
    ///         Ok(0)
    ///     }
    ///     fn write(&self, _out: &mut [u8]) -> Result<(), BuildError> {
    ///         Ok(())
    ///     }
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::ethernet::WriteFrameBody;
    /// use toyos_net_wire::ipv4::{Ipv4Builder, Ipv4Source, TrafficClass, Ttl};
    /// use toyos_net_wire::udp::UdpBuilder;
    /// use toyos_net_wire::Port;
    /// let builder = Ipv4Builder {
    ///     source: Ipv4Source::new(std::net::Ipv4Addr::new(192, 0, 2, 1)).unwrap(),
    ///     destination: std::net::Ipv4Addr::new(192, 0, 2, 2),
    ///     ttl: Ttl::DEFAULT,
    ///     traffic_class: TrafficClass::ZERO,
    ///     options: &[],
    ///     payload: UdpBuilder { source: Port::new(1).unwrap(), destination: Port::new(2).unwrap(), data: b"hi" },
    /// };
    /// let mut out = [0u8; 100];
    /// let _ = WriteFrameBody::write(&builder, &mut out);
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::checksum::PseudoHeader;
    /// use toyos_net_wire::ipv4::{Protocol, WritePayload};
    /// use toyos_net_wire::udp::UdpBuilder;
    /// use toyos_net_wire::Port;
    /// let udp = UdpBuilder { source: Port::new(1).unwrap(), destination: Port::new(2).unwrap(), data: b"hi" };
    /// let pseudo = PseudoHeader {
    ///     source: std::net::Ipv4Addr::new(192, 0, 2, 1),
    ///     destination: std::net::Ipv4Addr::new(192, 0, 2, 2),
    ///     protocol: Protocol::Tcp,
    ///     length: 100,
    /// };
    /// let _ = udp.write(&pseudo, &mut [0xAA; 100]);
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::checksum::PseudoHeader;
    /// use toyos_net_wire::ipv4::{Payload, Protocol};
    /// use toyos_net_wire::udp::UdpBuilder;
    /// use toyos_net_wire::Port;
    /// fn send<P: Payload>(payload: P, out: &mut [u8]) {
    ///     let pseudo = PseudoHeader {
    ///         source: std::net::Ipv4Addr::new(192, 0, 2, 1),
    ///         destination: std::net::Ipv4Addr::new(192, 0, 2, 2),
    ///         protocol: Protocol::Tcp,
    ///         length: 100,
    ///     };
    ///     let _ = payload.write(&pseudo, out);
    /// }
    /// let udp = UdpBuilder { source: Port::new(1).unwrap(), destination: Port::new(2).unwrap(), data: b"hi" };
    /// send(udp, &mut [0xAA; 100]);
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_wire::ethernet::FrameBody;
    /// fn frame<B: FrameBody>(body: &B, out: &mut [u8]) {
    ///     let _ = body.write(out);
    /// }
    /// ```
    #[allow(non_camel_case_types)]
    pub struct write_payload_and_write_frame_body_are_sealed;
}
