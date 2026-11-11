//! MEASUREMENT ONLY, never landed: where a spawn-and-wait's time goes, stage
//! by stage, on one timeline: this process's own stamps between the SDK calls
//! a spawn is made of, and the kernel's `Mark` records and scheduling events
//! from the diary, read on the same clock.
//!
//! Targets: `tiny` and `large` as osbench's `procs` writes them into `/tmp`
//! (the same bytes), `std` (`/system/bin/osbench nop`, off ROOT) and `tmpstd`
//! (the same binary copied to `/tmp`).

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::files::{self, At, Open};
use toyos::spawn::{Parent, Spawn, Start};
use toyos::syscap::{SysCap, TraceCursor};
use toyos::AsHandle;
use toyos_abi::clock;
use toyos_abi::handle::Rights;
use toyos_abi::trace::{Kind, TraceRecord, NO_THREAD};

const LARGE: usize = 32 << 20;
const REPS: usize = 60;
const WARM: usize = 6;

const EXIT_CODE: [u8; 11] = [0xbf, 0x48, 0x00, 0x00, 0x00, 0x31, 0xf6, 0x0f, 0x05, 0x0f, 0x0b];
const _: () = assert!(toyos_abi::syscall::SYS_EXIT == 0x48);

/// osbench's `procs::program`, byte for byte.
fn program(payload: usize) -> Vec<u8> {
    const PT_LOAD: u32 = 1;
    const PT_GNU_STACK: u32 = 0x6474_e551;
    const EHDR: usize = 64;
    const PHDR: usize = 56;
    let phnum = if payload > 0 { 3 } else { 2 };
    let code_at = EHDR + PHDR * phnum;
    let first = code_at + EXIT_CODE.len();
    let mut out = vec![0u8; first];
    let put = |out: &mut Vec<u8>, at: usize, bytes: &[u8]| out[at..at + bytes.len()].copy_from_slice(bytes);
    put(&mut out, 0, b"\x7fELF\x02\x01\x01");
    put(&mut out, 16, &3u16.to_le_bytes());
    put(&mut out, 18, &62u16.to_le_bytes());
    put(&mut out, 20, &1u32.to_le_bytes());
    put(&mut out, 24, &(code_at as u64).to_le_bytes());
    put(&mut out, 32, &(EHDR as u64).to_le_bytes());
    put(&mut out, 52, &(EHDR as u16).to_le_bytes());
    put(&mut out, 54, &(PHDR as u16).to_le_bytes());
    put(&mut out, 56, &(phnum as u16).to_le_bytes());
    put(&mut out, 58, &64u16.to_le_bytes());
    let header = |out: &mut Vec<u8>, i: usize, kind: u32, flags: u32, offset: u64, vaddr: u64, size: u64| {
        let at = EHDR + PHDR * i;
        put(out, at, &kind.to_le_bytes());
        put(out, at + 4, &flags.to_le_bytes());
        put(out, at + 8, &offset.to_le_bytes());
        put(out, at + 16, &vaddr.to_le_bytes());
        put(out, at + 24, &vaddr.to_le_bytes());
        put(out, at + 32, &size.to_le_bytes());
        put(out, at + 40, &size.to_le_bytes());
        put(out, at + 48, &(if kind == PT_LOAD { 0x1000u64 } else { 16 }).to_le_bytes());
    };
    header(&mut out, 0, PT_LOAD, 0b101, 0, 0, first as u64);
    header(&mut out, 1, PT_GNU_STACK, 0b110, 0, 0, 0);
    if payload > 0 {
        header(&mut out, 2, PT_LOAD, 0b100, 0x1000, 0x20_0000, payload as u64);
    }
    put(&mut out, code_at, &EXIT_CODE);
    if payload > 0 {
        out.resize(0x1000, 0);
        out.extend((0..payload).map(|i| (i % 249) as u8 | 1));
    }
    out
}

fn cap() -> &'static SysCap {
    static CAP: OnceLock<SysCap> = OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability"))
}

fn read_to_now(cursor: &mut TraceCursor<Vec<u64>>) -> (Vec<TraceRecord>, u64) {
    let mut out = Vec::new();
    let mut lost = 0;
    let mut raw = vec![TraceRecord::EMPTY; 1024];
    loop {
        let n = cap().trace(cursor, &mut raw).expect("the capability reads the diary");
        lost += cursor.lost();
        out.extend_from_slice(&raw[..n]);
        if n < raw.len() {
            return (out, lost);
        }
    }
}

fn now() -> u64 {
    clock::stamp_ns()
}

/// One point on the timeline: when, and what.
#[derive(Clone, Debug)]
struct Point {
    ns: u64,
    name: String,
}

struct Rep {
    points: Vec<Point>,
    /// The child's own accounting.
    child: toyos_abi::syscall::ProcessStats,
    /// The diary's records of the rep, for the one raw timeline printed.
    raw: Vec<TraceRecord>,
    child_pid: u32,
    cpu_before: u32,
    cpu_after: u32,
}

struct Target {
    name: &'static str,
    path: String,
    argv: Vec<u8>,
}

fn p(points: &mut Vec<Point>, name: &str) {
    points.push(Point { ns: now(), name: name.into() });
}

/// One spawn-and-wait through the SDK's own pieces, stamped between them.
fn staged(t: &Target, cursor: &mut TraceCursor<Vec<u64>>) -> Rep {
    let (_, _) = read_to_now(cursor);
    let cpu_before = toyos_abi::syscall::current_cpu();
    let mut pts = Vec::new();
    let null = |rights: Rights| toyos::null(rights.union(Rights::DUP).union(Rights::TRANSFER).union(Rights::WAIT)).expect("null");
    p(&mut pts, "u.begin");
    let n0 = null(Rights::READ);
    let n1 = null(Rights::WRITE);
    let n2 = null(Rights::WRITE);
    p(&mut pts, "u.nulls_made");
    let image = files::read_image(t.path.as_bytes()).unwrap_or_else(|e| panic!("read_image {}: {e:?}", t.path));
    p(&mut pts, "u.image_read");
    // A replica of what `Helper::spawn` does before its syscall, timed alone.
    let block = toyos::startup::Block::new(&t.argv, b"PWD=/\0", None).expect("block");
    p(&mut pts, "u.replica_block");
    let program = toyos::elf::program::lay_out(image.bytes(), toyos::elf::Machine::RUNNING, image.handle())
        .expect("lay_out")
        .starting_with(&block);
    std::hint::black_box(program.maps().len());
    p(&mut pts, "u.replica_layout");
    drop(block);
    p(&mut pts, "u.replica_block_dropped");
    let fds = [
        (0u32, n0.as_handle(), toyos::fd::Kind::Handle),
        (1u32, n1.as_handle(), toyos::fd::Kind::Handle),
        (2u32, n2.as_handle(), toyos::fd::Kind::Handle),
    ];
    let request = Spawn {
        program: &t.path,
        image: None,
        image_held: false,
        launcher: None,
        pager: None,
        argv: &t.argv,
        env: b"",
        cwd: "/",
        fds: &fds,
        endow: Vec::new(),
        provided: Vec::new(),
        parent: Parent::Caller,
        independent: false,
    };
    let Start::Helper(helper) = request.start().expect("start") else { unreachable!() };
    p(&mut pts, "u.spawn_call");
    let process = helper.spawn(Some(&image)).expect("spawn");
    p(&mut pts, "u.spawn_returned");
    drop(image);
    p(&mut pts, "u.image_dropped");
    drop((n0, n1, n2));
    p(&mut pts, "u.nulls_dropped");
    let end = process.wait().expect("wait");
    p(&mut pts, "u.waited");
    let child = process.stats().expect("stats");
    drop(process);
    p(&mut pts, "u.process_dropped");
    assert!(matches!(end, toyos_abi::end::End::Exited(0)), "{} ended {end:?}", t.name);
    let cpu_after = toyos_abi::syscall::current_cpu();
    // The reap follows in an idle loop; give it its moment before the read.
    std::thread::sleep(std::time::Duration::from_millis(2));
    let (raw, lost) = read_to_now(cursor);
    if lost != 0 {
        println!("stages {}: the diary lost {lost} records in one rep", t.name);
    }
    let page = clock::page();
    let own = std::process::id();
    let begin = pts[0].ns;
    let end_ns = pts.last().unwrap().ns;
    for r in &raw {
        let ns = page.stamp_of(r.stamp);
        if ns < begin {
            continue;
        }
        let who = if r.pid == child.pid {
            "c"
        } else if r.pid == own {
            "p"
        } else {
            continue;
        };
        let name = match Kind::from_u16(r.kind) {
            Some(Kind::Mark) => format!("k.{who}.m{}", r.data),
            Some(Kind::Pick) => format!("k.{who}.pick"),
            Some(Kind::Wake) => format!("k.{who}.wake"),
            Some(Kind::ParkCommit) => format!("k.{who}.park"),
            Some(Kind::Retire) => format!("k.{who}.retire"),
            Some(Kind::Migrate) => format!("k.{who}.migrate"),
            Some(Kind::Adopt) => format!("k.{who}.adopt"),
            _ => continue,
        };
        // The parent's records after the rep's end are the sleep's.
        if who == "p" && ns > end_ns {
            continue;
        }
        pts.push(Point { ns, name });
    }
    pts.sort_by_key(|pt| pt.ns);
    Rep { points: pts, child, raw, child_pid: child.pid, cpu_before, cpu_after }
}

/// std's `Command`, as osbench's `proc.spawn_wait.*` times it.
fn command(t: &Target) -> u64 {
    let mut args = t.argv.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| std::str::from_utf8(a).unwrap());
    let _argv0 = args.next();
    let began = now();
    let status = Command::new(&t.path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", t.path));
    let took = now() - began;
    assert!(status.success());
    took
}

fn p50(v: &mut [u64]) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}

/// The first point named `name`, at or after `from`.
fn at(points: &[Point], name: &str) -> Option<u64> {
    points.iter().find(|pt| pt.name == name).map(|pt| pt.ns)
}

fn last(points: &[Point], name: &str) -> Option<u64> {
    points.iter().rev().find(|pt| pt.name == name).map(|pt| pt.ns)
}

fn report(t: &Target, reps: &[Rep], command_ns: &mut [u64], command_after: &mut [u64]) {
    let n = t.name;
    let total: Vec<u64> = reps.iter().map(|r| at(&r.points, "u.process_dropped").unwrap() - at(&r.points, "u.begin").unwrap()).collect();
    let mut sorted = total.clone();
    println!(
        "stages {n}: reps={} staged_total_p50={} command_p50={} command_after_p50={} command_min={}",
        reps.len(),
        p50(&mut sorted),
        p50(command_ns),
        p50(command_after),
        command_ns[0],
    );
    // Each stage: from one named point to another, p50 and min over the reps
    // that have both.
    let spans: &[(&str, &str, &str, bool)] = &[
        ("user: three null handles made", "u.begin", "u.nulls_made", false),
        ("user: read_image (open, IMAGE or read whole, close)", "u.nulls_made", "u.image_read", false),
        ("user: [replica] startup block made", "u.image_read", "u.replica_block", false),
        ("user: [replica] lay_out", "u.replica_block", "u.replica_layout", false),
        ("user: [replica] startup block dropped", "u.replica_layout", "u.replica_block_dropped", false),
        ("user: Helper::spawn whole", "u.spawn_call", "u.spawn_returned", false),
        ("  user before SYS_SPAWN (block, lay_out, dups)", "u.spawn_call", "k.p.m100", false),
        ("  kernel: SYS_SPAWN whole", "k.p.m100", "k.p.m104", false),
        ("    spawn_program (ranges copied in, objects found)", "k.p.m100", "k.p.m101", false),
        ("    place, pager, path, name, labels", "k.p.m101", "k.p.m102", false),
        ("    build_child_handles", "k.p.m102", "k.p.m103", false),
        ("    admission, charge", "k.p.m103", "k.p.m110", false),
        ("    Space::new_user", "k.p.m110", "k.p.m111", false),
        ("    clock page, lay ranges (tables built)", "k.p.m111", "k.p.m112", false),
        ("    stack reserve", "k.p.m112", "k.p.m113", false),
        ("    image record, kernel stack", "k.p.m113", "k.p.m114", false),
        ("    process object, notice", "k.p.m114", "k.p.m115", false),
        ("    commit (handles move)", "k.p.m115", "k.p.m116", false),
        ("    process data, thread data", "k.p.m116", "k.p.m117", false),
        ("    table insert, enqueue", "k.p.m117", "k.p.m118", false),
        ("    the spawn's log line", "k.p.m118", "k.p.m119", false),
        ("    return path", "k.p.m119", "k.p.m104", false),
        ("  user after SYS_SPAWN", "k.p.m104", "u.spawn_returned", false),
        ("user: image dropped (unmap, close)", "u.spawn_returned", "u.image_dropped", false),
        ("user: nulls dropped", "u.image_dropped", "u.nulls_dropped", false),
        ("user: wait whole", "u.nulls_dropped", "u.waited", false),
        ("user: stats, process handle dropped", "u.waited", "u.process_dropped", false),
        ("child: enqueue to its first pick", "k.p.m118", "k.c.pick", false),
        ("child: first pick to exit entered (its whole run)", "k.c.pick", "k.c.m300", false),
        ("child: exit: claim, retire, walk", "k.c.m300", "k.c.m301", false),
        ("child: exit: leave to table section", "k.c.m301", "k.c.m320", false),
        ("child: exit: teardown_resources (handles closed)", "k.c.m320", "k.c.m321", false),
        ("child: exit: releases run", "k.c.m321", "k.c.m322", false),
        ("child: exit: bookkeeping and the exit's log line", "k.c.m322", "k.c.m323", false),
        ("child: exit: stats, torn down", "k.c.m323", "k.c.m324", false),
        ("child: exit: publish (the waiter is posted)", "k.c.m324", "k.c.m325", false),
        ("child: exit whole (m300 to m330)", "k.c.m300", "k.c.m330", false),
        ("parent: publish to its wake record", "k.c.m324", "k.p.wake", true),
        ("parent: its wake record to its pick", "k.p.wake", "k.p.pick", true),
        ("parent: its pick to wait returning in the kernel", "k.p.pick", "k.p.m401", true),
        ("parent: wait entered to exit entered by the child", "k.p.m400", "k.c.m300", false),
        ("parent: child's exit whole to wait returned to user", "k.c.m330", "u.waited", false),
        ("whole: spawn returned to child's exit entered", "u.spawn_returned", "k.c.m300", false),
    ];
    for (label, from, to, use_last) in spans {
        let mut v: Vec<i64> = reps
            .iter()
            .filter_map(|r| {
                let a = if *use_last { last(&r.points, from) } else { at(&r.points, from) }?;
                let b = if *use_last { last(&r.points, to) } else { at(&r.points, to) }?;
                Some(b as i64 - a as i64)
            })
            .collect();
        if v.is_empty() {
            println!("stages {n}: [{label}] absent");
            continue;
        }
        v.sort_unstable();
        println!("stages {n}: [{label}] p50={} min={} max={} n={}", v[v.len() / 2], v[0], v[v.len() - 1], v.len());
    }
    // The child's faults, from its marks: count, and the time inside the handler.
    let mut counts = Vec::new();
    let mut inside = Vec::new();
    for r in reps {
        let mut open = None;
        let (mut count, mut ns) = (0u64, 0u64);
        for pt in &r.points {
            match pt.name.as_str() {
                "k.c.m210" => open = Some(pt.ns),
                "k.c.m211" => {
                    if let Some(a) = open.take() {
                        count += 1;
                        ns += pt.ns - a;
                    }
                }
                _ => {}
            }
        }
        counts.push(count);
        inside.push(ns);
    }
    let stat = |f: fn(&toyos_abi::syscall::ProcessStats) -> u64| {
        let mut v: Vec<u64> = reps.iter().map(|r| f(&r.child)).collect();
        p50(&mut v)
    };
    println!(
        "stages {n}: child faults p50={} fault_handler_ns p50={} stats: wall={} cpu={} faults={} fault_ns={} syscalls={} syscall_ns={} runqueue_wait={} peak_memory={}",
        p50(&mut counts),
        p50(&mut inside),
        stat(|s| s.wall_ns),
        stat(|s| s.cpu_ns),
        stat(|s| s.faults),
        stat(|s| s.fault_ns),
        stat(|s| s.syscall_total),
        stat(|s| s.syscall_total_ns),
        stat(|s| s.runqueue_wait_ns),
        stat(|s| s.peak_memory),
    );
    // Where the child ran, against where the parent was.
    let mut placed: BTreeMap<String, u32> = BTreeMap::new();
    for r in reps {
        let child_cpu = r.raw.iter().find(|x| x.pid == r.child_pid && x.kind == Kind::Pick as u16).map(|x| x.cpu);
        let key = match child_cpu {
            Some(c) if u32::from(c) == r.cpu_before => "child on the parent's CPU".to_string(),
            Some(_) => "child on another CPU".to_string(),
            None => "no pick read".to_string(),
        };
        *placed.entry(key).or_default() += 1;
        if r.cpu_before != r.cpu_after {
            *placed.entry("parent moved CPU across the rep".into()).or_default() += 1;
        }
    }
    println!("stages {n}: placement {placed:?}");
    // The space's drop and the reap, wherever they ran (any pid).
    let mut drops = Vec::new();
    let mut reap = Vec::new();
    for r in reps {
        let page = clock::page();
        let mut open: BTreeMap<(u16, u32), u64> = BTreeMap::new();
        for x in &r.raw {
            if x.kind != Kind::Mark as u16 {
                continue;
            }
            let ns = page.stamp_of(x.stamp);
            match x.data {
                510 | 501 => {
                    open.insert((x.cpu, x.data), ns);
                }
                511 => {
                    if let Some(a) = open.remove(&(x.cpu, 510)) {
                        drops.push(ns - a);
                    }
                }
                502 => {
                    if let Some(a) = open.remove(&(x.cpu, 501)) {
                        reap.push(ns - a);
                    }
                }
                _ => {}
            }
        }
    }
    if !drops.is_empty() {
        let k = drops.len();
        println!("stages {n}: address space drops seen={k} p50={} max={}", p50(&mut drops), drops[k - 1]);
    }
    if !reap.is_empty() {
        let k = reap.len();
        println!("stages {n}: reap drops seen={k} p50={} max={}", p50(&mut reap), reap[k - 1]);
    }
    // One raw timeline: the rep whose total is the median.
    let mut order: Vec<usize> = (0..reps.len()).collect();
    order.sort_by_key(|&i| total[i]);
    let median = &reps[order[order.len() / 2]];
    let begin = median.points[0].ns;
    let page = clock::page();
    println!("timeline {n}: the median rep, child pid {}, parent on cpu {} then {}", median.child_pid, median.cpu_before, median.cpu_after);
    let mut lines: Vec<(u64, String)> = median.points.iter().filter(|pt| pt.name.starts_with("u.")).map(|pt| (pt.ns, pt.name.clone())).collect();
    let mut faults = 0;
    for x in &median.raw {
        let ns = page.stamp_of(x.stamp);
        if ns < begin {
            continue;
        }
        // Faults are counted above; a std child's would be hundreds of lines.
        if x.kind == Kind::Mark as u16 && (x.data == 210 || x.data == 211) {
            faults += 1;
            if faults > 12 {
                continue;
            }
        }
        if matches!(Kind::from_u16(x.kind), Some(Kind::TimerArm | Kind::TimerStop)) {
            continue;
        }
        let who = if x.pid == NO_THREAD { "-".to_string() } else { format!("{}/{}", x.pid, x.tid) };
        lines.push((ns, format!("cpu{} {who} kind={} data={}", x.cpu, x.kind, x.data)));
    }
    lines.sort_by_key(|l| l.0);
    for (ns, what) in lines.iter().take(260) {
        println!("timeline {n}: +{} {what}", ns - begin);
    }
}

/// `read_image`'s own parts for a served file, each timed alone.
fn image_parts(t: &Target) {
    let mut open_ns = Vec::new();
    let mut stat_ns = Vec::new();
    let mut create_ns = Vec::new();
    let mut read_ns = Vec::new();
    let mut close_ns = Vec::new();
    let mut drop_ns = Vec::new();
    let mut whole = Vec::new();
    let mut whole_drop = Vec::new();
    let mut immutable = 0;
    for i in 0..(WARM + 30) {
        let a = now();
        let fd = files::open(At::Cwd, t.path.as_bytes(), Open { read: true, cloexec: true, ..Open::default() }).expect("open");
        let b = now();
        let stat = files::fstat(fd).expect("fstat");
        let c = now();
        let size = stat.size as usize;
        let memory = toyos::shm::SharedMemory::create(size).expect("create");
        let d = now();
        // SAFETY: a fresh mapping of `size` bytes this process alone holds.
        let bytes = unsafe { core::slice::from_raw_parts_mut(memory.as_ptr(), size) };
        let mut done = 0;
        while done < size {
            done += files::read(fd, &mut bytes[done..]).expect("read");
        }
        let e = now();
        // SAFETY: the descriptor opened above, named nowhere else.
        unsafe { toyos::fd::close(fd) }.expect("close");
        let f = now();
        drop(memory);
        let g = now();
        let image = files::read_image(t.path.as_bytes()).expect("read_image");
        let h = now();
        if image.as_immutable().is_some() {
            immutable += 1;
        }
        drop(image);
        let k = now();
        if i >= WARM {
            open_ns.push(b - a);
            stat_ns.push(c - b);
            create_ns.push(d - c);
            read_ns.push(e - d);
            close_ns.push(f - e);
            drop_ns.push(g - f);
            whole.push(h - g);
            whole_drop.push(k - h);
        }
    }
    println!(
        "image {}: bytes={} immutable_answers={immutable} open={} fstat={} shm_create={} read_whole={} close={} shm_drop={} | read_image={} its_drop={}",
        t.name,
        std::fs::metadata(&t.path).map(|m| m.len()).unwrap_or(0),
        p50(&mut open_ns),
        p50(&mut stat_ns),
        p50(&mut create_ns),
        p50(&mut read_ns),
        p50(&mut close_ns),
        p50(&mut drop_ns),
        p50(&mut whole),
        p50(&mut whole_drop),
    );
}

fn main() {
    let dir = "/tmp/spawn-stages";
    let _ = std::fs::create_dir_all(dir);
    std::fs::write(format!("{dir}/tiny"), program(0)).expect("write tiny");
    std::fs::write(format!("{dir}/large"), program(LARGE)).expect("write large");
    // osbench where the image has it (the bench's), toybox elsewhere.
    let (std_path, std_arg) = if std::fs::metadata("/system/bin/osbench").is_ok() { ("/system/bin/osbench", "nop") } else { ("/system/bin/toybox", "echo") };
    std::fs::copy(std_path, format!("{dir}/osbench")).expect("copy the std program");
    let argv = |parts: &[&str]| parts.iter().flat_map(|p| p.bytes().chain([0])).collect::<Vec<u8>>();
    let targets = [
        Target { name: "tiny", path: format!("{dir}/tiny"), argv: argv(&[&format!("{dir}/tiny")]) },
        Target { name: "large", path: format!("{dir}/large"), argv: argv(&[&format!("{dir}/large")]) },
        Target { name: "std", path: std_path.into(), argv: argv(&[std_path, std_arg]) },
        Target { name: "tmpstd", path: format!("{dir}/osbench"), argv: argv(&[&format!("{dir}/osbench"), std_arg]) },
    ];
    let words = cap().trace_cursor_words().expect("the capability sizes a cursor");
    let mut cursor = TraceCursor::new(vec![0; words]);
    println!("spawn_stages: cpus={} std={std_path}", toyos_abi::syscall::cpu_count());
    for t in &targets {
        // std's Command first, undisturbed by anything this job adds.
        let mut cmd: Vec<u64> = (0..WARM + REPS).map(|_| command(t)).skip(WARM).collect();
        let reps: Vec<Rep> = (0..WARM + REPS).map(|_| staged(t, &mut cursor)).skip(WARM).collect();
        let mut cmd_after: Vec<u64> = (0..WARM + REPS).map(|_| command(t)).skip(WARM).collect();
        cmd.sort_unstable();
        report(t, &reps, &mut cmd, &mut cmd_after);
        image_parts(t);
    }
    let _ = std::fs::remove_dir_all(dir);
    println!("spawn_stages: done");
}
