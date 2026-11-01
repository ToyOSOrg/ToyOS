//! Dropping the last handle to a shared region must give the pages back.
//!
//! `SYS_RELEASE_SHARED` unmapped the caller and dropped it from the region's
//! `allowed` list, and stopped there: the "is anyone left?" test lived only in
//! `cleanup_process`, so nothing freed a region at close time and soundserver's
//! per-client ring stayed resident until some unrelated process exited. There
//! is no release call now — a region's life is its handle count, and the
//! zero-handle hook is where the mappings go.
//!
//! Two directions, because a reclaim rule that is too eager is worse than one
//! that never fires: a region whose maker has dropped its handle but which it
//! **sent** to somebody must survive, and that somebody must still be able to
//! read it. Run as `... donor` this binary is that maker.

use std::io::{BufRead, BufReader, Write};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::census::Census;
use toyos::shm::SharedMemory;
use toyos::{namespace, port};
use toyos_abi::handle::ObjectKind;
use toyos_abi::syscall::SVC_LABEL;

#[path = "../census_wait.rs"]
mod census_wait;

const SELF_PATH: &str = "/system/bin/test_rs_shm_release_reclaims";
const PAYLOAD: &[u8] = b"sent-before-the-maker-let-go";
/// Sixteen rather than one because the arrival check has to be able to fail: a
/// loop that made no region would leave nothing for the reclaim assertion to
/// be about.
const ROUNDS: usize = 16;
const REGION: usize = 4096;
const SERVICE: &str = "region";

fn main() {
    if std::env::args().nth(1).as_deref() == Some("donor") {
        return donor();
    }

    // **Per kind, and not the machine's free memory.** `SYS_SYSINFO` answers
    // for the whole machine, so a verdict taken from it is sound only while
    // nothing else in the guest holds or releases a page across the window, and
    // nothing orders that. A live object count moves only when somebody makes
    // or releases one, and it is exact: a leak of one region is `+1`.
    let start = census_wait::settled();

    let mut regions = Vec::new();
    for _ in 0..ROUNDS {
        let region = SharedMemory::create(REGION).expect("a region of our own");
        region.bytes().write(0, &[0xA5]);
        regions.push(region);
    }
    let held = Census::now();

    // Non-vacuity: if the instrument could not see sixteen regions arrive, it
    // cannot see them come back either, and the reclaim assertion below would
    // pass on a kernel that frees nothing.
    let taken = held.kind(ObjectKind::SharedMem).saturating_sub(start.kind(ObjectKind::SharedMem));
    assert!(
        taken >= ROUNDS as u64,
        "{ROUNDS} regions were allocated and the live SharedMem count moved {taken}: \
         first {start}, then {held}"
    );

    drop(regions);
    census_wait::released_to(&start);
    // The other direction. The donor makes a region, sends it here, and drops
    // its own handle before this process has mapped anything — nobody has the
    // region mapped at that moment, and it is still this process's.
    let (acceptor, connector) = port::create().expect("the kernel refused a port");
    let ns = namespace::build()
        .add(SERVICE, &connector)
        .finish()
        .expect("the kernel refused a namespace");
    let mut donor = Command::new(SELF_PATH)
        .arg("donor")
        .endow(SVC_LABEL, ns.into())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn donor");

    let conn = acceptor.accept().expect("the donor connects");
    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    rx.recv(&conn).expect("the donor announces its region");
    let [sent] = rx.take_handles::<1>().expect("the donor sent the region with the frame");

    // Only now, after the donor has let go — its line says so.
    let mut out = BufReader::new(donor.stdout.take().expect("donor stdout"));
    let mut line = String::new();
    out.read_line(&mut line).expect("donor release line");
    assert_eq!(line.trim(), "released", "the donor did not report letting go");

    let region = SharedMemory::lent(sent, REGION)
        .expect("a region this process holds a handle to was reclaimed under it");
    assert_eq!(
        region.bytes().sub(0, PAYLOAD.len()).to_vec(),
        PAYLOAD,
        "the sent region no longer holds the donor's payload"
    );
    drop(region);

    let mut donor_in = donor.stdin.take().expect("donor stdin");
    writeln!(donor_in, "quit").expect("tell the donor to quit");
    drop(donor_in);
    assert!(donor.wait().expect("wait donor").success(), "donor exited nonzero");

    println!(
        "dropped regions reclaimed ({taken} live SharedMem out and back); \
         a sent region survived its maker letting go"
    );
}

fn donor() {
    let conn = toyos::endow::service(SERVICE).expect("donor: the port it was given");
    let region = SharedMemory::create(REGION).expect("donor: a region of its own");
    region.bytes().write(0, PAYLOAD);

    let shared = region.share().expect("donor: a second handle");
    conn.signal_with(1, [shared]).expect("donor: send the region");

    // The maker lets go while the peer has not mapped yet: no process has this
    // region mapped, and the in-flight handle is the only thing keeping it —
    // exactly the state a too-eager reclaim would free.
    drop(region);
    println!("released");
    std::io::stdout().flush().expect("donor: flush");

    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}
