//! The stack netd runs: Netstack3's core, its bindings, and the one Ethernet
//! device the manifest gave this program.
//!
//! **IPv4 alone, configured from the lease.** The device comes up with IPv4
//! enabled and no address, IPv6 disabled, IGMP on for the multicast DNS group,
//! and address conflict detection off; [`Net::apply`] writes a lease's
//! address, its subnet route and its default route together, and clears all
//! of them together.

use std::time::Instant;

use net_types::ethernet::Mac;
use net_types::ip::{AddrSubnet, Ipv4, Ipv4Addr, Ipv6, Mtu, Subnet};
use net_types::{SpecifiedAddr, UnicastAddr};
use netstack3_core::device::{
    DeviceId, EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice, MaxEthernetFrameSize,
    RecvEthernetFrameMeta,
};
use netstack3_core::ip::{
    IidSecret, IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
};
use netstack3_core::routes::{AddableEntry, AddableMetric, Generation, RawMetric};
use netstack3_core::{CoreApi, NetworkParsingContext, StackState, StackStateBuilder};
use packet::Buf;

use crate::stack::{Bindings, DeviceName, DeviceState, KernelRng, VecAllocator};

/// The device's routing metric: its routes' own, since it is the only one.
const METRIC: RawMetric = RawMetric(100);

/// An address as a lease grants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub addr: [u8; 4],
    pub prefix: u8,
    pub router: Option<[u8; 4]>,
}

/// Why a lease's address could not be written.
#[derive(Debug)]
pub struct Unusable(pub String);

pub struct Net {
    core: StackState<Bindings>,
    pub bindings: Bindings,
    device: EthernetDeviceId<Bindings>,
    address: Option<Address>,
}

impl Net {
    /// The stack over a device whose link address is `mac`, its clock's zero
    /// at `epoch`.
    pub fn new(mac: [u8; 6], epoch: Instant) -> Self {
        let mut bindings = Bindings::new(epoch);
        let mut builder = StackStateBuilder::default();
        // IPv6 is off, and the builder asks for its SLAAC secret all the same.
        builder.ipv6_builder().slaac_stable_secret_key(IidSecret::new_random(&mut KernelRng));
        let core = builder.build_with_ctx(&mut bindings);
        let mac = UnicastAddr::new(Mac::new(mac))
            .unwrap_or_else(|| panic!("netd: the NIC's address {mac:02x?} is not a unicast one"));
        let properties = EthernetCreationProperties {
            mac,
            max_frame_size: MaxEthernetFrameSize::from_mtu(Mtu::new(1500)).expect("1500 is an Ethernet MTU"),
            tx_offload_spec: Default::default(),
        };
        let device = core
            .api(&mut bindings)
            .device::<EthernetLinkDevice>()
            .add_device(DeviceName, properties, METRIC, DeviceState, VecAllocator);
        let mut net = Self { core, bindings, device, address: None };
        let id = net.device_id();
        let v4 = Ipv4DeviceConfigurationUpdate {
            ip_config: IpDeviceConfigurationUpdate {
                ip_enabled: Some(true),
                gmp_enabled: Some(true),
                dad_transmits: Some(None),
                ..Default::default()
            },
            ..Default::default()
        };
        let _: Ipv4DeviceConfigurationUpdate = net
            .api()
            .device_ip::<Ipv4>()
            .update_configuration(&id, v4)
            .expect("netd: a fresh device takes IPv4");
        let v6 = Ipv6DeviceConfigurationUpdate {
            ip_config: IpDeviceConfigurationUpdate { ip_enabled: Some(false), ..Default::default() },
            ..Default::default()
        };
        let _: Ipv6DeviceConfigurationUpdate = net
            .api()
            .device_ip::<Ipv6>()
            .update_configuration(&id, v6)
            .expect("netd: a fresh device takes IPv6 off");
        net
    }

    pub fn api(&mut self) -> CoreApi<'_, &mut Bindings> {
        self.core.api(&mut self.bindings)
    }

    pub fn device_id(&self) -> DeviceId<Bindings> {
        self.device.clone().into()
    }

    /// The address the device holds.
    pub fn address(&self) -> Option<Address> {
        self.address
    }

    /// Hand the stack one received frame.
    pub fn receive(&mut self, frame: &[u8]) {
        let meta = RecvEthernetFrameMeta {
            device_id: self.device.clone(),
            parsing_context: NetworkParsingContext::default(),
            gso_info: None,
        };
        let buf = Buf::new(frame.to_vec(), ..);
        self.core.api(&mut self.bindings).device::<EthernetLinkDevice>().receive_frame(meta, buf);
    }

    /// Fire every timer due now.
    pub fn fire_timers(&mut self) {
        loop {
            let now = self.bindings.now();
            let Some((dispatch, id)) = self.bindings.timers.pop_due(now) else { break };
            self.api().handle_timer(dispatch, id);
        }
        self.bindings.sweep_deferred();
    }

    /// When the soonest timer is due.
    pub fn next_timer(&self) -> Option<Instant> {
        self.bindings.timers.next().map(|at| at.at(self.bindings.epoch()))
    }

    /// Write `address` into the device — the address, the subnet it is on and
    /// the default route through its router — or clear all three.
    ///
    /// **A lease is the network's word, so an address the stack refuses is
    /// refused here by name** and leaves the device with none.
    pub fn apply(&mut self, address: Option<Address>) -> Result<(), Unusable> {
        let id = self.device_id();
        if let Some(old) = self.address.take() {
            let addr = SpecifiedAddr::new(Ipv4Addr::new(old.addr)).expect("a held address is specified");
            let _ = self
                .api()
                .device_ip::<Ipv4>()
                .del_ip_addr(&id, addr)
                .expect("netd: the address it holds is on its device");
        }
        let main = self.api().routes::<Ipv4>().main_table_id();
        let Some(new) = address else {
            self.api().routes::<Ipv4>().set_routes(&main, Vec::new());
            return Ok(());
        };
        let subnet = AddrSubnet::<Ipv4Addr>::new(Ipv4Addr::new(new.addr), new.prefix)
            .map_err(|e| Unusable(format!("{}/{} is no address on a subnet: {e:?}", show(new.addr), new.prefix)))?;
        let gateway = new
            .router
            .map(|r| SpecifiedAddr::new(Ipv4Addr::new(r)).ok_or_else(|| Unusable("the lease names 0.0.0.0 as its router".into())))
            .transpose()?;
        self.api()
            .device_ip::<Ipv4>()
            .add_ip_addr_subnet(&id, subnet)
            .map_err(|e| Unusable(format!("the stack refused {}/{}: {e:?}", show(new.addr), new.prefix)))?;
        let metric = self.api().device_ip::<Ipv4>().get_routing_metric(&id);
        let on_link = AddableEntry::without_gateway(subnet.subnet(), id.clone(), AddableMetric::MetricTracksInterface);
        let mut routes = vec![on_link.resolve_metric(metric).with_generation(Generation::initial())];
        if let Some(gateway) = gateway {
            let default = Subnet::new(Ipv4Addr::new([0; 4]), 0).expect("the default subnet");
            let via = AddableEntry::with_gateway(default, id.clone(), gateway, AddableMetric::MetricTracksInterface);
            routes.push(via.resolve_metric(metric).with_generation(Generation::initial()));
        }
        self.api().routes::<Ipv4>().set_routes(&main, routes);
        self.address = Some(new);
        Ok(())
    }
}

/// An address the way a log line spells one.
pub fn show(addr: [u8; 4]) -> String {
    std::net::Ipv4Addr::from(addr).to_string()
}
