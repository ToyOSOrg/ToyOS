//! DATA's partition claim held across its file server's restart.
//!
//! `tests/fsdclaimcase` arms DATA's server with `--let-go-at-read`: the first
//! read-only open of [`LET_GO`] lets its partition go and is refused, and this
//! client's next request ends the server. This binary takes the claim in
//! between, so init's restart of the role finds it held, and holds it until
//! init has answered: a new open waits in the role's port until init either
//! closes it, which answers Gone, or starts a server that answers it. The host
//! judges init's and fsd's lines (`tests/common/storage.rs`'s `fsd_claim_held`).

use std::fs::File;
use std::io::ErrorKind;

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::PartitionDev;
use toyos_abi::part::PartGuid;
use toyos_abi::syscall::SYSCAP_LABEL;

/// Mirrored in `tests/common/storage.rs`: the DATA partition on the crafted
/// stick.
const DATA: &str = "7E2B4C6D-8F1A-4B3C-9D5E-6F7A8B9C0D1E";
/// Mirrored in `tests/fsdclaimcase/system.toml`.
const LET_GO: &str = "/home/fsd_let_go";
/// A name on DATA nothing makes: asked only to reach its server.
const AFTER: &str = "/home/fsd_claim_held";

fn main() {
    let cap: SysCap =
        Endowments::get().take(SYSCAP_LABEL).expect("the test estate is endowed a device-minting capability");
    let data = PartGuid::parse(DATA).expect("DATA is a GUID");

    match File::open(LET_GO) {
        Err(e) => println!("fs_claim_held: the server let its partition go, and the open was refused ({e})"),
        Ok(_) => panic!("the open of {LET_GO} was answered: the server did not let its partition go"),
    }
    let held = cap
        .claim_partition::<PartitionDev>(data)
        .unwrap_or_else(|e| panic!("DATA's claim, which its server let go, was refused: {e:?}"));
    println!("fs_claim_held: holding DATA's claim");

    match File::open(AFTER) {
        Err(e) => println!("fs_claim_held: the request the server ends under was refused ({e})"),
        Ok(_) => panic!("the server answered the request it was armed to end under"),
    }
    match File::open(AFTER) {
        Err(e) if e.kind() == ErrorKind::StaleNetworkFileHandle => {
            println!("fs_claim_held: init did not start DATA's server again, and {AFTER} answers Gone ({e})")
        }
        Err(e) => panic!("{AFTER} was refused {e} ({:?}), not Gone", e.kind()),
        Ok(_) => panic!("{AFTER} was answered while this process held DATA's claim: a server runs without it"),
    }
    drop(held);
    println!("fs_claim_held: PASS");
}
