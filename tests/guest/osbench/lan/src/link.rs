//! Which of this machine's links one of its own addresses is on, described
//! as Linux's sysfs and `/proc/net/wireless` give it. Every other system
//! answers that it reads none. A description names no interface or address.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// The links a result can be named for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Wired,
    Wifi,
}

impl Link {
    pub fn name(self) -> &'static str {
        match self {
            Link::Wired => "wired",
            Link::Wifi => "wifi",
        }
    }
}

/// The link a datagram to `peer` would leave by. Connecting a UDP socket
/// chooses its route and sends nothing.
pub fn toward(peer: IpAddr) -> Result<(Link, String), String> {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(|e| format!("no socket: {e}"))?;
    probe.connect((peer, 9)).map_err(|e| format!("no route: {e}"))?;
    of(probe.local_addr().map_err(|e| format!("no local address: {e}"))?.ip())
}

/// The link holding `local`, and its description: kind, driver, and the
/// wired link's speed or the wireless one's signal.
#[cfg(target_os = "linux")]
pub fn of(local: IpAddr) -> Result<(Link, String), String> {
    let name = linux::interface_holding(local)?;
    let at = std::path::Path::new("/sys/class/net").join(&name);
    let Ok(driver) = std::fs::read_link(at.join("device/driver")) else {
        return Err("the address is on an interface no device drives".into());
    };
    let driver = driver.file_name().map_or_else(|| "unnamed".into(), |n| n.to_string_lossy().into_owned());
    if at.join("wireless").exists() || at.join("phy80211").exists() {
        let signal = std::fs::read_to_string("/proc/net/wireless")
            .ok()
            .and_then(|table| signal_dbm(&table, &name))
            .map_or_else(|| "unread".into(), |dbm| dbm.to_string());
        Ok((Link::Wifi, format!("wireless driver={driver} signal_dbm={signal}")))
    } else {
        let speed = std::fs::read_to_string(at.join("speed")).map_or_else(|_| "unread".into(), |s| s.trim().to_string());
        Ok((Link::Wired, format!("wired driver={driver} speed_mbps={speed}")))
    }
}

#[cfg(not(target_os = "linux"))]
pub fn of(_local: IpAddr) -> Result<(Link, String), String> {
    Err("this system's links are not read".into())
}

/// `interface`'s signal level in dBm from a `/proc/net/wireless` table, whose
/// rows are `<name>: <status> <link> <level> <noise> ...`, each number
/// perhaps ending in `.`.
pub fn signal_dbm(table: &str, interface: &str) -> Option<i32> {
    table.lines().find_map(|line| {
        let (name, fields) = line.trim_start().split_once(':')?;
        if name != interface {
            return None;
        }
        fields.split_whitespace().nth(2)?.trim_end_matches('.').parse().ok()
    })
}

#[cfg(target_os = "linux")]
mod linux {
    use std::net::IpAddr;

    /// The name of the interface holding `local`.
    pub fn interface_holding(local: IpAddr) -> Result<String, String> {
        let IpAddr::V4(local) = local else { return Err("the address is IPv6".into()) };
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: `list` is a valid out-pointer; the list is freed below.
        if unsafe { libc::getifaddrs(&mut list) } != 0 {
            return Err(format!("getifaddrs: {}", std::io::Error::last_os_error()));
        }
        let mut found = None;
        let mut at = list;
        while !at.is_null() {
            // SAFETY: a node of the list getifaddrs made, alive until it is freed.
            let ifa = unsafe { &*at };
            // SAFETY: a non-null `ifa_addr` of family AF_INET is a sockaddr_in.
            let v4 = (!ifa.ifa_addr.is_null() && unsafe { (*ifa.ifa_addr).sa_family } == libc::AF_INET as libc::sa_family_t)
                .then(|| unsafe { (*ifa.ifa_addr.cast::<libc::sockaddr_in>()).sin_addr.s_addr });
            if v4 == Some(u32::from(local).to_be()) {
                // SAFETY: an interface's name is a NUL-terminated string.
                found = Some(unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned());
                break;
            }
            at = ifa.ifa_next;
        }
        // SAFETY: the list getifaddrs made, read no more.
        unsafe { libc::freeifaddrs(list) };
        found.ok_or_else(|| "no interface holds the address".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE
 face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22
wlp1s0: 0000   58.  -52.  -256        0      0      0      0      0        0
 wlan1: 0000   70   -40   -256        0      0      0      0      0        0
";

    #[test]
    fn a_wireless_interfaces_signal_is_its_level_column() {
        assert_eq!(signal_dbm(TABLE, "wlp1s0"), Some(-52));
        assert_eq!(signal_dbm(TABLE, "wlan1"), Some(-40));
    }

    #[test]
    fn an_interface_the_table_does_not_list_has_no_signal() {
        assert_eq!(signal_dbm(TABLE, "wlp1"), None);
        assert_eq!(signal_dbm("wlp1s0: 0000 58.", "wlp1s0"), None);
    }
}
