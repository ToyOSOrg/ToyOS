//! Every refusal, every drop and every ordinary event has a counter named `<area>.<name>`. A
//! refusal of legacy or insecure input, and every change of a neighbour's MAC, is also an event
//! naming the rule and the peer. A rule a `toyos-net-wire` parse refused is counted under that
//! reason's own name.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use toyos_net_wire::icmp::IcmpError;

use crate::{limits, Event, IfIndex, Peer, Refusal};

toyos_net_wire::counters! {
    ClockRegressed = "clock.regressed";
    UnknownInterface = "ip.unknown-interface";
    AddrPrefixInvalid = "addr.prefix-invalid";
    AddrNotUnicast = "addr.not-unicast";
    AddrDuplicate = "addr.duplicate";
    AddrTooMany = "addr.too-many";
    AddrUnknown = "addr.unknown";
    RouteGatewayInvalid = "route.gateway-invalid";
    RouteGatewayIsLocal = "route.gateway-is-local";
    RouteGatewayOffLink = "route.gateway-off-link";
    RouteTooManyGateways = "route.too-many-gateways";
    RouteGatewayWithdrawn = "route.gateway-withdrawn";
    RouteGatewaySwitched = "route.gateway-switched";
    RouteInvalidDestination = "route.invalid-destination";
    RouteLocalDestination = "route.local-destination";
    RouteAmbiguousInterface = "route.ambiguous-interface";
    RouteNone = "route.none";
    RouteNoSourceAddress = "route.no-source-address";
    EthIpv6 = "eth.ipv6";
    EthUnknownType = "eth.unknown-type";
    EthNotForUs = "eth.not-for-us";
    EthVlan = "eth.vlan";
    EthOwnSource = "eth.own-source";
    IpUnicastInLinkBroadcast = "ip.unicast-in-link-broadcast";
    IpUnicastInLinkMulticast = "ip.unicast-in-link-multicast";
    IpMartianDestination = "ip.martian-destination";
    IpTentativeDestination = "ip.tentative-destination";
    IpNotForUs = "ip.not-for-us";
    IpInvalidSource = "ip.invalid-source";
    IpOwnSource = "ip.own-source";
    IpFragment = "ip.fragment", logged;
    IpSourceRoute = "ip.source-route", logged;
    IpProtocol = "ip.protocol";
    IpTcpNotUnicast = "ip.tcp-not-unicast";
    IpExceedsMtu = "ip.exceeds-mtu";
    IpBroadcastNotPermitted = "ip.broadcast-not-permitted", logged;
    IpControlQueueFull = "ip.control-queue-full";
    IpAcquisitionAdmitted = "ip.acquisition-admitted";
    IpEventOverflow = "ip.event-overflow";
    ArpOwnSender = "arp.own-sender";
    ArpInvalidSenderAddress = "arp.invalid-sender-address";
    ArpGroupSenderHardware = "arp.group-sender-hardware";
    ArpSenderOffLink = "arp.sender-off-link";
    ArpNotForUs = "arp.not-for-us";
    ArpMacChanged = "arp.mac-changed", logged;
    ArpOverrideLocked = "arp.override-locked", logged;
    ArpRequestsSent = "arp.requests-sent";
    ArpRepliesSent = "arp.replies-sent";
    NbPendingOverflow = "nb.pending-overflow";
    NbPendingFull = "nb.pending-full";
    NbPendingDropped = "nb.pending-dropped";
    NbPendingEvicted = "nb.pending-evicted";
    NbFailedRefused = "nb.failed-refused";
    NbTableFull = "nb.table-full";
    NbResolved = "nb.resolved";
    NbFailed = "nb.failed";
    NbUnreachable = "nb.unreachable";
    AcdConflict = "acd.conflict", logged;
    AcdDefended = "acd.defended", logged;
    AcdRateLimited = "acd.rate-limited";
    AcdVerified = "acd.verified";
    IcmpEchoToGroup = "icmp.echo-to-group";
    IcmpErrorToGroup = "icmp.error-to-group";
    IcmpEchoReply = "icmp.echo-reply";
    IcmpEchoReplyDropped = "icmp.echo-reply-dropped";
    IcmpErrorSuppressed = "icmp.error-suppressed";
    IcmpErrorRateLimited = "icmp.error-rate-limited";
    IcmpQuoteNotOurs = "icmp.quote-not-ours";
    IcmpQuoteNonInitialFragment = "icmp.quote-non-initial-fragment";
    IcmpQuoteIcmp = "icmp.quote-icmp";
    IcmpQuoteIgmp = "icmp.quote-igmp";
    IcmpQuoteOtherProtocol = "icmp.quote-other-protocol";
    IcmpQuoteShort = "icmp.quote-short";
    IcmpRedirect = "icmp.redirect", logged;
    IcmpTimestampRequest = IcmpError::TimestampRequest.name(), logged;
    IcmpSourceQuench = IcmpError::SourceQuench.name(), logged;
    IcmpEchoRepliesSent = "icmp.echo-replies-sent";
    IcmpErrorsSent = "icmp.errors-sent";
    IgmpV1Query = "igmp.v1-query", logged;
    IgmpQueryNoRouterAlert = "igmp.query-no-router-alert", logged;
    IgmpGeneralQueryDestination = "igmp.general-query-destination", logged;
    IgmpQueryDestination = "igmp.query-destination";
    IgmpQueryOtherGroup = "igmp.query-other-group";
    IgmpTooManyGroups = "igmp.too-many-groups";
    IgmpIgnored = "igmp.ignored";
    IgmpReportsSent = "igmp.reports-sent";
    IgmpModeV2 = "igmp.mode-v2";
    IgmpModeV3 = "igmp.mode-v3";
}

#[derive(Debug, Default)]
pub(crate) struct Log {
    pub counters: Counters,
    pub wire: BTreeMap<&'static str, u64>,
    events: Vec<Event>,
    refusals: usize,
}

impl Log {
    pub fn count(&mut self, counter: Counter) {
        self.counters.add(counter, 1);
    }

    pub fn wire(&mut self, name: &'static str) {
        let n = self.wire.entry(name).or_insert(0);
        *n = n.saturating_add(1);
    }

    /// Counts a refusal, and names it for the log when its rule is one the log carries. Past
    /// [`limits::EVENTS`] undrained refusals one is counted and not named: a peer never grows the
    /// list.
    pub fn refuse(&mut self, rule: Counter, iface: IfIndex, peer: Peer) {
        self.count(rule);
        if !rule.logged() {
            return;
        }
        if self.refusals >= limits::EVENTS {
            self.count(Counter::IpEventOverflow);
            return;
        }
        self.refusals = self.refusals.saturating_add(1);
        self.events.push(Event::Refused(Refusal { rule, iface, peer }));
    }

    /// A result the shell acts on; each is bounded by the work that made it.
    pub fn event(&mut self, event: Event) {
        self.events.push(event);
    }

    pub fn drain(&mut self) -> alloc::vec::Drain<'_, Event> {
        self.refusals = 0;
        self.events.drain(..)
    }
}
