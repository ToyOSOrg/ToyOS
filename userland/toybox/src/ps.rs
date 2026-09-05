//! Every process in the machine, by name.
//!
//! **The endowment is the whole of the authority**, as `/system/bin/shutdown`'s is.
//! `/system/bin/ps` is `/system/bin/toybox` under another name, so what this holds is what
//! the image's `[programs.toybox]` row declares — a config that does not name
//! `roster` there builds an image whose `ps` says it cannot and changes nothing
//! else. Nothing here asks for the capability: it is either in the endowment
//! table `/system/bin/init` filled at spawn or it does not exist for this process.
//!
//! `free` is the other half of the same syscall and needs none of this: the
//! machine header is ambient.
//!
//! **`%CPU` here and the taskbar's are the same quantity**, a share of one CPU
//! over a window this process measures for itself — see [`SAMPLE_NS`]. The
//! rows also do not sum to the machine, and the last line says by how much.

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::system;

const HEADER: usize = system::SYSINFO_HEADER_SIZE;
const ENTRY: usize = system::SYSINFO_ENTRY_SIZE;

/// How far apart the two roster samples `%CPU` is a rate over are taken.
///
/// **`%CPU` is a rate and the roster carries no start time**, so a cumulative
/// `cpu_ns` over the machine's uptime is a share of neither the process's life
/// nor this instant, and it disagrees with every reader that takes a window —
/// the compositor taskbar among them. Two samples is the only rate this ABI
/// can express, and this is that taskbar's one-second delta shortened to what
/// a command may keep somebody waiting.
const SAMPLE_NS: u64 = 100_000_000;

/// One live thread, as the roster describes it.
struct Row {
    pid: u32,
    tid: u32,
    state: u8,
    is_thread: bool,
    memory: u64,
    cpu_ns: u64,
    name: String,
}

/// The machine's uptime, the CPU nanoseconds it has ever run, and every row.
///
/// `None` is a capability carrying no `ROSTER`: the kernel answers the ambient
/// header alone, which is short of one entry and says so by its length.
fn sample(cap: &SysCap, buf: &mut [u8]) -> Option<(u64, u64, Vec<Row>)> {
    let n = cap.roster(buf);
    if n < HEADER {
        return None;
    }
    let uptime_ns = u64::from_le_bytes(buf[24..32].try_into().unwrap());
    let total_cpu_ns = u64::from_le_bytes(buf[32..40].try_into().unwrap());
    let mut rows = Vec::new();
    let mut pos = HEADER;
    while pos + ENTRY <= n {
        let name_bytes = &buf[pos + 32..pos + 60];
        let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(28);
        rows.push(Row {
            pid: u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()),
            tid: u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()),
            state: buf[pos + 8],
            is_thread: buf[pos + 9] != 0,
            memory: u64::from_le_bytes(buf[pos + 16..pos + 24].try_into().unwrap()),
            cpu_ns: u64::from_le_bytes(buf[pos + 24..pos + 32].try_into().unwrap()),
            name: std::str::from_utf8(&name_bytes[..name_len]).unwrap_or("?").to_string(),
        });
        pos += ENTRY;
    }
    Some((uptime_ns, total_cpu_ns, rows))
}

fn format_cpu_time(ns: u64) -> String {
    let total_ms = ns / 1_000_000;
    let secs = total_ms / 1000;
    let ms = total_ms % 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{mins}:{secs:02}.{ms:03}")
    } else {
        format!("{secs}.{ms:03}")
    }
}

fn state_str(s: u8) -> &'static str {
    match s {
        0 => "R",
        1 => "R+",
        3 => "Z",
        _ => "S",
    }
}

pub fn main(_args: Vec<String>) {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("ps: this program was endowed no system capability");
        std::process::exit(1);
    };
    let mut buf = vec![0u8; HEADER + ENTRY * 128];
    let Some((first_uptime, _, first)) = sample(&cap, &mut buf) else {
        eprintln!("ps: refused — this capability carries no ROSTER");
        std::process::exit(1);
    };
    std::thread::sleep(std::time::Duration::from_nanos(SAMPLE_NS));
    let Some((uptime_ns, total_cpu_ns, rows)) = sample(&cap, &mut buf) else {
        eprintln!("ps: refused — this capability carries no ROSTER");
        std::process::exit(1);
    };
    let window_ns = uptime_ns.saturating_sub(first_uptime);

    // No PPID column: a process has no parent. What started it gave it what it
    // holds and kept a handle, and neither of those is a number the table has.
    println!("{:>5} {:>3} {:>2} {:>8} {:>5} {:>5}  {}",
        "PID", "TID", "S", "CPU", "%CPU", "MEM", "NAME");

    for row in &rows {
        // A thread younger than the window has no earlier reading to be a rate
        // against, and a share of a window it did not live through is not one.
        let before = first.iter().find(|r| r.pid == row.pid && r.tid == row.tid);
        let cpu_pct = match before {
            Some(before) if window_ns > 0 => {
                format!("{}%", row.cpu_ns.saturating_sub(before.cpu_ns) * 100 / window_ns)
            }
            _ => String::from("-"),
        };

        let mem = if row.memory >= 1 << 20 {
            format!("{}M", row.memory >> 20)
        } else if row.memory >= 1 << 10 {
            format!("{}K", row.memory >> 10)
        } else {
            format!("{}B", row.memory)
        };

        let kind = if row.is_thread { " (thread)" } else { "" };

        println!("{:>5} {:>3} {:>2} {:>8} {:>5} {:>5}  {}{}",
            row.pid, row.tid, state_str(row.state), format_cpu_time(row.cpu_ns),
            cpu_pct, mem, row.name, kind);
    }

    // The rows do not add up to the machine and the difference has an owner:
    // a reaped process's time stays in the total forever and leaves every row.
    let live: u64 = rows.iter().map(|r| r.cpu_ns).sum();
    println!(
        "{} of {} CPU belongs to processes that have been reaped",
        format_cpu_time(total_cpu_ns.saturating_sub(live)),
        format_cpu_time(total_cpu_ns),
    );
}
