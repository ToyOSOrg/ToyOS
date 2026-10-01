//! ARP reception (§7): `wire.md` §4.3's policy, then §7.2's classification in order. Only our own
//! need to send, and a request for one of our addresses from an on-link host, create an entry;
//! any other packet can confirm, assert a MAC into STALE, or be ignored, and a MAC change is
//! always logged (§7.3).

use toyos_net_wire::addr::is_host;
use toyos_net_wire::arp::{Arp, Operation};
use toyos_net_wire::ethernet::MacClass;

use crate::counters::Counter;
use crate::iface::{Cx, Interface};
use crate::{acd, egress, nud};

pub(crate) fn receive(i: &mut Interface, cx: &mut Cx<'_>, arp: &Arp) {
    let (sender, mac, target) = (arp.sender_ip, arp.sender_mac, arp.target_ip);
    if mac.class() != MacClass::Individual {
        return cx.log.count(Counter::ArpGroupSenderHardware);
    }
    let probe = arp.operation == Operation::Request && sender.is_unspecified();
    if !(probe || is_host(sender)) {
        return cx.log.count(Counter::ArpInvalidSenderAddress);
    }
    if mac == i.mac.get() {
        return cx.log.count(Counter::ArpOwnSender);
    }
    if i.usable().any(|a| a.cidr.is_edge(sender)) {
        return cx.log.count(Counter::ArpInvalidSenderAddress);
    }
    if !probe && i.owns(sender) {
        return acd::conflict(i, cx, sender, mac);
    }
    if probe && i.owns(target) && !i.is_usable(target) {
        return acd::conflict(i, cx, target, mac);
    }
    let for_us = arp.operation == Operation::Request && i.is_usable(target);
    let mut noticed = for_us;
    if !probe {
        if !i.on_link(sender) {
            cx.log.count(Counter::ArpSenderOffLink);
            noticed = true;
        } else if i.neighbours.contains_key(&sender) {
            if arp.operation == Operation::Reply && nud::solicits(i, sender) && i.is_usable(target) {
                nud::confirm(i, cx, sender, mac);
            } else {
                nud::assert(i, cx, sender, mac);
            }
            noticed = true;
        } else if for_us {
            nud::learn(i, cx, sender, mac);
        }
    }
    if for_us {
        egress::reply(cx, arp, target);
    }
    if !noticed {
        cx.log.count(Counter::ArpNotForUs);
    }
}
