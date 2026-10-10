//! A block service's grants, asked of diskserver serving the disk this
//! machine booted from: this job holds the controller's claim and the block
//! port's acceptor, starts diskserver with both as the supervisor starts it,
//! and mints every grant on the acceptor as the supervisor mints a file
//! server's.
//!
//! - One partition's grant lists that partition alone, opens it, and is
//!   refused another partition of the same disk by name — one nothing holds,
//!   so nothing but the grant stands in the way.
//! - A type's grant lists and opens its type's partition and is refused one
//!   of another type.
//! - A session whose grant does not write is refused a write, and reads.
//! - The port's own connector, which carries no grant, lists and opens
//!   nothing.
//! - The partition the machine runs from is held, whatever reaches it.

use std::os::toyos::process::CommandExt;
use std::process::Command;

use diskserver::{Error, Outcome, Session};
use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::namespace::{self, Namespace};
use toyos::port::{self, Connector};
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_abi::inventory::{RawRecord, Record, Role};
use toyos_abi::syscall::{DeviceRequest, DEV_PREFIX, SERVE_PREFIX};
use toyos_blockring::wire::{Grant, Refusal, Scope};
use toyos_blockring::PORT;

/// The controller diskserver's row names, which this boot starts no row for.
const CONTROLLER: &str = "pci:1b36:0010";

fn names(connector: &Connector) -> Namespace {
    namespace::build().add(PORT, connector).finish().expect("partition_grant: a namespace of one connector")
}

fn listed(connector: &Connector) -> Result<Vec<[u8; 16]>, Error> {
    diskserver::list(&names(connector), PORT).map(|all| all.into_iter().map(|l| l.unique).collect())
}

fn open(connector: &Connector, guid: [u8; 16]) -> Result<Session, Error> {
    Session::open(names(connector), PORT, guid)
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let records: Vec<Record> = cap.records(|n| vec![RawRecord::EMPTY; n]).expect("partition_grant: the inventory");
    let loaded = |role: Role| {
        records
            .iter()
            .find_map(|r| match r {
                Record::Loaded(l) if l.role == role => Some(l.unique_guid),
                _ => None,
            })
            .unwrap_or_else(|| panic!("partition_grant: the loader named no {role:?}"))
    };
    let (log, boot, root) = (loaded(Role::Log), loaded(Role::Boot), loaded(Role::Root));

    let Some(DeviceRequest::Pci(id)) = DeviceRequest::parse(CONTROLLER) else { unreachable!("a PCI request") };
    let claim: toyos::Device = cap.claim_pci(id).expect("partition_grant: the NVMe controller nothing else claims");
    let (acceptor, bare) = port::create().expect("partition_grant: the block port");
    let served = toyos_abi::syscall::dup(acceptor.as_handle()).expect("partition_grant: the acceptor for diskserver");
    let mut text = [0u8; toyos_abi::part::GUID_TEXT_LEN];
    let running = toyos_abi::part::PartGuid(root).write_text(&mut text).to_string();
    let mut server = Command::new("/system/bin/diskserver")
        .args(["--running", &running])
        .endow(&format!("{DEV_PREFIX}{CONTROLLER}"), claim.into_raw().0)
        .endow(&format!("{SERVE_PREFIX}{PORT}"), served.0)
        .spawn()
        .expect("partition_grant: diskserver");
    let mint = |scope: Scope, writes: bool| {
        acceptor.mint(&Grant { scope, writes }.encode()).expect("partition_grant: a grant minted")
    };

    let logs = mint(Scope::Unique(log), true);
    assert_eq!(listed(&logs), Ok(vec![log]), "the log's grant listed more than the log");
    let held = open(&logs, log).expect("partition_grant: the log's grant opens the log");
    assert_eq!(
        open(&logs, boot).err(),
        Some(Error::Refused(Refusal::NotGranted)),
        "the log's grant opened the boot volume, which nothing holds"
    );
    drop(held);
    println!("partition_grant: the log's grant opened the log and was refused the boot volume NotGranted");

    let data = mint(Scope::Kind(toyos_gpt::Guid::TOYOS_DATA.0), true);
    let [data_part] = listed(&data).expect("partition_grant: DATA's grant lists")[..] else {
        panic!("partition_grant: DATA's grant lists other than one partition")
    };
    let held = open(&data, data_part).expect("partition_grant: DATA's grant opens DATA");
    assert_eq!(open(&data, log).err(), Some(Error::Refused(Refusal::NotGranted)), "DATA's grant opened the log");
    drop(held);
    println!("partition_grant: DATA's grant opened DATA and was refused the log NotGranted");

    let boots = mint(Scope::Unique(boot), false);
    let mut volume = open(&boots, boot).expect("partition_grant: the boot volume's grant opens it");
    let (read, first) = volume.read(0, 1).expect("partition_grant: a read of the boot volume");
    let first = first.filter(|_| read == Outcome::Done).expect("partition_grant: the boot volume's first block");
    // The bytes it holds: a write let through changes nothing.
    assert_eq!(
        volume.write(0, &first),
        Ok(Outcome::Invalid),
        "a session whose grant does not write wrote the boot volume"
    );
    drop(volume);
    println!("partition_grant: the boot volume's read-only grant read it and was refused a write Invalid");

    assert_eq!(listed(&bare), Err(Error::Refused(Refusal::NotGranted)), "the port's own connector listed");
    assert_eq!(open(&bare, log).err(), Some(Error::Refused(Refusal::NotGranted)), "the port's own connector opened");
    println!("partition_grant: the port's own connector was refused a listing and an open NotGranted");

    let roots = mint(Scope::Unique(root), true);
    assert_eq!(open(&roots, root).err(), Some(Error::Refused(Refusal::Held)), "a grant opened the running ROOT");
    println!("partition_grant: the running ROOT was refused Held to the grant naming it");

    server.kill().expect("partition_grant: diskserver ends");
    server.wait().expect("partition_grant: diskserver reaped");
}
