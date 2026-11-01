//! A link that ends under a held stream and comes back: netstack keeps the
//! connections it carries across its link's end.
//!
//! This job claims the virtio NIC, which no row of its boot drives, starts
//! `/system/bin/virtio-net` on it and, once netstack has leased, has a line
//! answered on a stream to the harness. It then kills the driver, claims the
//! NIC again once the kernel has taken the claim back, starts the driver
//! again, and has a second line answered on the same stream once netstack
//! says the link ended and is open again; then the stream ends, each side's
//! FIN read by the other.

#[path = "../served_log.rs"]
mod served_log;
#[path = "../spare.rs"]
mod spare;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::os::toyos::process::CommandExt;
use std::process::{Child, Command};
use std::time::Duration;

use toyos::endow::{Endowments, DEV_PREFIX, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::PciId;

const DRIVER: &str = "/system/bin/virtio-net";
const NIC: PciId = PciId { vendor: 0x1af4, device: 0x1041 };
/// The label the driver looks its claim up by: its row's spelling.
const LABEL: &str = "pci:02:00:00/1af4:1041";

/// QEMU's user network's address of its host.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// A liveness ceiling on each wait, never a timing.
const BOUND: Duration = Duration::from_secs(90);

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|port| port.parse().ok())
        .expect("link_restart: the harness's port is this job's argument");
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("link_restart: test-runner endows a device-minting capability");
    let mut log = served_log::Log::open();

    let mut driver = start(&cap);
    log.until("netstack's lease", BOUND, |line| line.contains("netstack: ready, at most"));
    let mut stream = TcpStream::connect(SocketAddr::from((HOST, port))).expect("link_restart: a connect through netstack to the harness");
    stream.set_read_timeout(Some(BOUND)).expect("link_restart: a read timeout");
    answered(&mut stream, "before");
    println!("  PASS: a line answered on the stream over the first link");

    end(&mut driver);
    let mut driver = start(&cap);
    let mut ended = false;
    log.until("the link's end and a link opened after it", BOUND, |line| {
        ended |= line.contains("netstack: virtio-net's link ended: it hung up");
        ended && line.contains("netstack: virtio-net's link is open")
    });
    println!("  PASS: netstack said the link ended, and opened the restarted driver's");

    answered(&mut stream, "after");
    println!("  PASS: a line answered on the same stream over the second link");
    // The FIN both ways before the driver ends: one queued after the link is
    // gone would never leave.
    stream.shutdown(Shutdown::Write).expect("link_restart: the stream's FIN");
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).expect("link_restart: the harness's FIN");
    assert!(rest.is_empty(), "link_restart: the harness said {:?} after its last answer", String::from_utf8_lossy(&rest));
    end(&mut driver);
    println!("link_restart: a stream held across its link's end and a new driver's link");
}

/// `virtio-net` on a claim of the NIC, which nothing holds.
fn start(cap: &SysCap) -> Child {
    let pci = spare::of(cap, &[NIC]).expect("link_restart: the virtio NIC is held");
    let dev: PciDev = cap
        .claim_function(pci.arrival)
        .unwrap_or_else(|why| panic!("link_restart: the NIC's claim was refused: {why:?}"));
    Command::new(DRIVER)
        .endow(&format!("{DEV_PREFIX}{LABEL}"), dev.into())
        .spawn()
        .expect("link_restart: spawn virtio-net")
}

/// Kill the driver and wait for its end: once the wait returns, its claim is
/// back and can be minted again.
fn end(driver: &mut Child) {
    driver.kill().expect("link_restart: kill the driver");
    driver.wait().expect("link_restart: the driver's end");
}

/// Write `word` as a line and read the harness's `heard <word>` line back.
fn answered(stream: &mut TcpStream, word: &str) {
    stream.write_all(format!("{word}\n").as_bytes()).unwrap_or_else(|why| panic!("link_restart: the write of {word:?}: {why}"));
    let owed = format!("heard {word}\n");
    let mut heard = Vec::new();
    let mut byte = [0u8; 1];
    while heard.last() != Some(&b'\n') {
        match stream.read(&mut byte) {
            Ok(1) => heard.push(byte[0]),
            Ok(_) => panic!("link_restart: the harness hung up after {:?}", String::from_utf8_lossy(&heard)),
            Err(why) => panic!("link_restart: the answer to {word:?}: {why} after {:?}", String::from_utf8_lossy(&heard)),
        }
    }
    assert_eq!(String::from_utf8_lossy(&heard), owed, "link_restart: the harness's answer");
}
