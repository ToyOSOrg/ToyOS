//! `/system/bin/inspect [--json] [SELECTOR]` — what the machine's owners say
//! about themselves, now.
//!
//! The reader asks every owner in `toyos_inspect::OWNERS` whose root the
//! selector can reach, through the connector this process was given for that
//! owner's port, and prints the paths the selector matches as `path = value`
//! lines sorted by path, or as one JSON object. The grammar, the wire form and
//! the renderings are `toyos-inspect`'s, and the question put to one owner is
//! [`inspect::ask`]; this file is which owners are asked, and the kernel's two
//! roots, the inventory and the counters.
//!
//! **An owner this process holds no connector for is a refusal, not a gap**:
//! it is named on stderr and the run exits 2, so a pipe never mistakes a partial
//! answer for a whole one. The same for an owner whose port is closed, one that
//! does not answer within [`inspect::ask`]'s bound, and one whose answer is not
//! a snapshot for its own root. Exit 1 is every owner answering and nothing
//! matching, as `grep` says it.

use std::collections::BTreeMap;
use std::io::Write;

use inspect::ask;
use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters;
use toyos_abi::inventory::{RawRecord, Record};
use toyos_abi::syscall::SyscallError;
use toyos_inspect::{Invocation, Value};

const USAGE: &str = "usage: inspect [--json] [SELECTOR]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = match Invocation::parse(args.iter().map(String::as_str)) {
        Ok(run) => run,
        Err(why) => {
            eprintln!("inspect: {why}\n{USAGE}");
            std::process::exit(2);
        }
    };

    let mut found: BTreeMap<String, Value> = BTreeMap::new();
    let mut refused = false;
    for owner in toyos_inspect::OWNERS.into_iter().filter(|o| run.selector.reaches(o.root)) {
        match ask(owner) {
            Ok(snapshot) => {
                found.extend(snapshot.into_iter().filter(|(path, _)| run.selector.matches(path)))
            }
            Err(why) => {
                eprintln!("inspect: {}.*: {why}", owner.root);
                refused = true;
            }
        }
    }

    // Taken once: taking an endowment is a swap, and both kernel roots ask on it.
    let cap: Option<SysCap> = Endowments::get().take(SYSCAP_LABEL);
    let kernel_roots: [(&str, fn(&SysCap) -> Result<BTreeMap<String, Value>, String>); 2] =
        [(toyos_inspect::dev::ROOT, inventory), (toyos_inspect::kernel::ROOT, cpu_counters)];
    for (root, ask) in kernel_roots.into_iter().filter(|(root, _)| run.selector.reaches(root)) {
        let answer = cap.as_ref().ok_or_else(|| {
            "this program holds no system capability, so the kernel's answer is not its to read"
                .to_string()
        });
        match answer.and_then(ask) {
            Ok(paths) => {
                found.extend(paths.into_iter().filter(|(path, _)| run.selector.matches(path)))
            }
            Err(why) => {
                eprintln!("inspect: {root}.*: {why}");
                refused = true;
            }
        }
    }

    let out = if run.json {
        toyos_inspect::json(found.iter().map(|(p, v)| (p.as_str(), v)))
    } else {
        found.iter().map(|(p, v)| toyos_inspect::line(p, v) + "\n").collect()
    };
    let mut stdout = std::io::stdout().lock();
    // A reader whose pipe closed early (`| head`) has nobody left to tell.
    let _ = stdout.write_all(out.as_bytes()).and_then(|()| stdout.flush());

    std::process::exit(if refused {
        2
    } else if found.is_empty() {
        1
    } else {
        0
    });
}

/// The kernel's inventory, asked with this process's `SysCap`, and the
/// machine `SYS_SYSINFO`'s ambient header describes, as `dev.*` paths.
fn inventory(cap: &SysCap) -> Result<BTreeMap<String, Value>, String> {
    let records: Vec<Record> =
        cap.records(|n| vec![RawRecord::EMPTY; n]).map_err(|why| why.to_string())?;
    let mut header = [0u8; toyos::system::SYSINFO_HEADER_SIZE];
    if toyos::system::sysinfo(&mut header) != header.len() {
        return Err("the kernel wrote no machine header".to_string());
    }
    let machine = toyos_inspect::dev::Machine::from_header(&header);
    toyos_inspect::dev::render(&machine, &records).map_err(|why| why.to_string())
}

/// Every CPU's counters, asked with this process's `SysCap`, as `kernel.*`
/// paths. The CPU count does not change after boot, so one count sizes the read.
fn cpu_counters(cap: &SysCap) -> Result<BTreeMap<String, Value>, String> {
    let refused = |e: SyscallError| match e {
        SyscallError::PermissionDenied => {
            "the kernel refused: this program's capability does not carry `counters`".to_string()
        }
        e => format!("the counters would not read: {e:?}"),
    };
    let mut raw = vec![counters::RawRecord::EMPTY; cap.counters(&mut []).map_err(refused)?];
    let n = cap.counters(&mut raw).map_err(refused)?;
    let records = raw[..n]
        .iter()
        .map(|r| counters::Record::decode(r).map_err(|why| format!("a record does not decode: {why:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    toyos_inspect::kernel::render(&records).map_err(|why| why.to_string())
}
