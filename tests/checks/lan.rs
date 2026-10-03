//! `lan_dhcp_lease`'s judge over the T14's own talking boot, planted as the
//! loop writes one and read as a run reads one.

use super::*;
use toyos_build::metal::{READBACK_BOOT, READBACK_STREAM, READBACK_TALK};

/// The T14's `lantalkcase` boot, verbatim: the card's hand-over, what netd
/// said of its MAC, its link and its lease, and init's stop.
const LOG: &str = r"[2026-10-03 06:50:58 1.192 cpu0] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
{2026-10-03 06:51:02 5.727 netd} netd: MAC 38:f3:ab:35:37:3b
[2026-10-03 06:51:05 8.517 cpu4] pcidev: slot 0 took its first message on vector 0x28
{2026-10-03 06:51:05 8.517 netd} netd: I219: link up at 1000 Mb/s full duplex, 2789 ms after the driver came up
{2026-10-03 06:51:16 19.070 netd} netd: DHCP: lease 192.168.1.48/24 from 192.168.1.1, gateway 192.168.1.1, dns [194.230.55.96 212.98.37.130], 13342 ms after netd came up
{2026-10-03 06:51:16 19.070 netd} netd: ready, at most 103 piped connections (4 MiB each of 16020 MiB total)
{2026-10-03 06:51:17 20.351 init} init: power: the machine stops, and logd makes the log whole first (Reboot)
";

/// That boot's `boot.txt`, verbatim: the loop that wrote it also pinged the
/// address Ubuntu held on this MAC, `192.168.1.46`.
const BOOT: &str = "back_secs 70\nstick_secs 0\nmachine_vendor LENOVO\nmachine_product 20W0003AMZ\n\
                    machine_bios N34ET71W (1.71 )\nping_addr 192.168.1.46\n\
                    wire_mac 38:f3:ab:35:37:3b\nclock_skew 1\nping_secs 64\nping_at 1791010315\n";

/// That boot's `talk.txt`, verbatim.
const TALK: &str = "talk_peer 192.168.1.48\ntalk_ping yes\ntalk_exec_status 0\n\
                    talk_exec_stdout \"the T14 answers over its own cable\\n\"\ntalk_exec_ms 86\n\
                    talk_stream_end open\ntalk_reboot accepted\n";

fn judged(log: &str, boot: &str, talk: &str) -> Result<(), String> {
    let dir = toyos_tmpdir::TempDir::new("lan-readback");
    metal_checks::plant(&dir, lan::TALK_BOOT, "", log, None);
    let home = metal::at(&dir, lan::TALK_BOOT);
    for (name, text) in [(READBACK_BOOT, boot), (READBACK_TALK, talk), (READBACK_STREAM, log)] {
        fs::write(home.join(name), text).expect("a planted file");
    }
    let back = metal::read_readback(&dir, lan::TALK_BOOT).expect("a planted readback");
    metal_judge("lan_dhcp_lease")(&[&back])
}

/// The lease judged is the one this boot took: the T14's boot passes on the
/// address it answered the host at, whatever Ubuntu held before it, and reds
/// where netd's record names another address, where that address answered no
/// ping, where netd read another MAC than Ubuntu did, and with no lease.
pub fn the_lease_judged_is_this_boots_own() {
    assert_eq!(judged(LOG, BOOT, TALK), Ok(()));
    let refused = |what: &str, log: &str, boot: &str, talk: &str, says: &str| {
        let why = judged(log, boot, talk).expect_err(what);
        assert!(why.contains(says), "{what}: {why}");
    };
    refused(
        "a lease record naming an address the boot did not answer at",
        &LOG.replace("lease 192.168.1.48/24", "lease 192.168.1.46/24"),
        BOOT,
        TALK,
        "leased 192.168.1.46 and answered for its name at 192.168.1.48",
    );
    refused(
        "a leased address that answered no ping",
        LOG,
        BOOT,
        &TALK.replace("talk_ping yes", "talk_ping no"),
        "answered no ping of the host's",
    );
    refused(
        "a MAC that is not the one Ubuntu read",
        LOG,
        &BOOT.replace("wire_mac 38:f3:ab:35:37:3b", "wire_mac 38:f3:ab:35:37:3c"),
        TALK,
        "no \"netd: MAC 38:f3:ab:35:37:3c\" record",
    );
    let unleased: String = LOG.lines().filter(|l| !l.contains("DHCP: lease")).map(|l| format!("{l}\n")).collect();
    refused("a boot that took no lease", &unleased, BOOT, TALK, "took no address from its network");
}
