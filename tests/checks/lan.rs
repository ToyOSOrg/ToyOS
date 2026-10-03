//! `lan_dhcp_lease`'s judge over the T14's own talking boot, planted as the
//! loop writes one and read as a run reads one.

use super::*;
use toyos_build::metal::{READBACK_BOOT, READBACK_STREAM, READBACK_TALK};

/// The T14's `lantalkcase` boot: the card's hand-over, what netd said of its
/// MAC, its link and its lease, and init's stop. The MAC and the two resolvers
/// are stand-ins that name nobody; every other byte is the machine's.
const LOG: &str = r"[2026-10-03 10:20:01 1.192 cpu0] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
{2026-10-03 10:20:05 5.727 netd} netd: MAC 02:00:00:00:00:01
[2026-10-03 10:20:08 8.499 cpu4] pcidev: slot 0 took its first message on vector 0x28
{2026-10-03 10:20:08 8.499 netd} netd: I219: link up at 1000 Mb/s full duplex, 2772 ms after the driver came up
{2026-10-03 10:20:19 19.053 netd} netd: DHCP: lease 192.168.1.48/24 from 192.168.1.1, gateway 192.168.1.1, dns [192.0.2.53 198.51.100.53], 13326 ms after netd came up
{2026-10-03 10:20:19 19.053 netd} netd: ready, at most 103 piped connections (4 MiB each of 16022 MiB total)
{2026-10-03 10:20:20 20.249 init} init: power: the machine stops, and logd makes the log whole first (Reboot)
";

/// That boot's `boot.txt`, under the same stand-in.
const BOOT: &str = "back_secs 63\nstick_secs 0\nmachine_vendor LENOVO\nmachine_product 20W0003AMZ\n\
                    machine_bios N34ET71W (1.71 )\nwire_mac 02:00:00:00:00:01\n";

/// That boot's `talk.txt`, verbatim.
const TALK: &str = "talk_peer 192.168.1.48\ntalk_ping yes\ntalk_exec_status 0\n\
                    talk_exec_stdout \"the T14 answers over its own cable\\n\"\ntalk_exec_ms 77\n\
                    talk_stream_end open\ntalk_reboot accepted\n";

fn judged(log: &str, boot: &str) -> Result<(), String> {
    let dir = toyos_tmpdir::TempDir::new("lan-readback");
    metal_checks::plant(&dir, lan::TALK_BOOT, "", log, None);
    let home = metal::at(&dir, lan::TALK_BOOT);
    for (name, text) in [(READBACK_BOOT, boot), (READBACK_TALK, TALK), (READBACK_STREAM, log)] {
        fs::write(home.join(name), text).expect("a planted file");
    }
    let back = metal::read_readback(&dir, lan::TALK_BOOT).expect("a planted readback");
    metal_judge("lan_dhcp_lease")(&[&back])
}

/// The lease judged is the one this boot took: the T14's boot passes on the
/// address it answered the host at, and reds where netd's record names another
/// address, where netd read another MAC than Ubuntu did, and with no lease.
pub fn the_lease_judged_is_this_boots_own() {
    assert_eq!(judged(LOG, BOOT), Ok(()));
    let refused = |what: &str, log: &str, boot: &str, says: &str| {
        let why = judged(log, boot).expect_err(what);
        assert!(why.contains(says), "{what}: {why}");
    };
    refused(
        "a lease record naming an address the boot did not answer at",
        &LOG.replace("lease 192.168.1.48/24", "lease 192.168.1.46/24"),
        BOOT,
        "leased 192.168.1.46 and answered for its name at 192.168.1.48",
    );
    refused(
        "a MAC that is not the one Ubuntu read",
        LOG,
        &BOOT.replace("wire_mac 02:00:00:00:00:01", "wire_mac 02:00:00:00:00:02"),
        "no \"netd: MAC 02:00:00:00:00:02\" record",
    );
    let unleased: String =
        LOG.lines().filter(|l| !l.contains("DHCP: lease")).map(|l| format!("{l}\n")).collect();
    refused("a boot that took no lease", &unleased, BOOT, "took no address from its network");
}
