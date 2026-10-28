//! Three jobs contending for every CPU, and the share of the machine's work
//! each one got.
//!
//! - `build`: one process, one thread.
//! - `threaded`: one process, one thread per CPU.
//! - `helpers`: a parent keeping one helper process per CPU alive, each
//!   working [`HELPER_BURST`] and exiting, as a parallel build's compiler
//!   invocations do.
//!
//! Every worker runs the same compute chunk and counts the chunks it finished
//! in each window, on the clock every process shares. A job's share of a
//! window is its chunks over all three jobs' chunks: on CPUs of one kind that
//! is its share of CPU time, and `cpus_milli_p50` is that share of the
//! machine's CPUs.
//!
//! Each system runs it once with all three workloads in this program's own
//! group, `shared`, and once with a group per workload, `apart`; a line per
//! job, `fair.<grouping>.<job>`. On ToyOS the group is the Job object its
//! threads share CPU time under, and `apart` starts each workload in a Job of
//! its own (`SysCap::new_job`, which needs `Rights::JOBS`). On Linux `shared`
//! leaves them in this process's session and cgroup, where the fair unit is
//! the thread, and `apart` puts each in a transient `systemd-run --user
//! --scope` with equal `CPUWeight`, whose cgroup is the unit where the user's
//! manager has the `cpu` controller; Linux also runs `session`, a session
//! each, whose autogroup is the unit where autogroup applies. A `units` line
//! says which unit applied: `3xcgroup` is a cgroup per workload, `1xcgroup`
//! one cgroup for all three.

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::report::{condition, refuse, result};
use crate::sys::monotonic_ns;

/// The warm-up between the jobs' start and the first window, in which every
/// worker starts and the shares settle from where each job began.
const SETTLE: Duration = Duration::from_secs(2);
const WINDOW: Duration = Duration::from_secs(2);
const WINDOWS: usize = 5;
const HELPER_BURST: Duration = Duration::from_millis(100);

const JOBS: [&str; 3] = ["build", "threaded", "helpers"];

/// When the windows are, on [`monotonic_ns`]'s clock.
#[derive(Clone, Copy)]
struct Windows {
    origin: u64,
    width: u64,
    count: usize,
}

impl Windows {
    fn end(self) -> u64 {
        self.origin + self.width * self.count as u64
    }

    fn args(self) -> [String; 3] {
        [self.origin.to_string(), self.width.to_string(), self.count.to_string()]
    }

    fn parse(args: &[String]) -> Windows {
        let num = |i: usize| -> u64 { args.get(i).and_then(|a| a.parse().ok()).expect("the coordinator's windows") };
        Windows { origin: num(0), width: num(1), count: num(2) as usize }
    }

    /// Chunks until `until` or the last window's end, counted by the window
    /// each one finished in.
    fn work(self, until: u64) -> Vec<u64> {
        let stop = until.min(self.end());
        let mut counts = vec![0; self.count];
        loop {
            std::hint::black_box(crate::chunk());
            let now = monotonic_ns();
            if now >= stop {
                return counts;
            }
            if let Some(since) = now.checked_sub(self.origin) {
                counts[(since / self.width) as usize] += 1;
            }
        }
    }
}

fn add(into: &mut [u64], from: &[u64]) {
    for (a, b) in into.iter_mut().zip(from) {
        *a += b;
    }
}

fn counts_line(counts: &[u64]) -> String {
    counts.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

fn parse_counts(text: &str, count: usize) -> Option<Vec<u64>> {
    let counts: Vec<u64> = text.split(',').map(|c| c.parse().ok()).collect::<Option<_>>()?;
    (counts.len() == count).then_some(counts)
}

/// `osbench fair-job <kind> <origin> <width> <count>`: one job, which prints
/// `counts=<per window> helpers=<spawned> group=<key>` and exits.
pub fn job(args: &[String]) {
    let kind = args.first().map(String::as_str).expect("a job kind");
    let windows = Windows::parse(&args[1..]);
    let cpus = crate::cpus();
    let (counts, helpers) = match kind {
        "build" => (windows.work(u64::MAX), 0),
        "threaded" => {
            let workers: Vec<_> = (0..cpus).map(|_| thread::spawn(move || windows.work(u64::MAX))).collect();
            let mut counts = vec![0; windows.count];
            for worker in workers {
                add(&mut counts, &worker.join().expect("a worker panicked"));
            }
            (counts, 0)
        }
        "helpers" => helpers(windows, cpus),
        other => panic!("no job {other:?}"),
    };
    #[cfg(target_os = "linux")]
    let group = format!(" group={}", group_key());
    #[cfg(target_os = "toyos")]
    let group = String::new();
    println!("counts={} helpers={helpers}{group}", counts_line(&counts));
}

/// One helper process per CPU in flight until the windows end.
fn helpers(windows: Windows, cpus: usize) -> (Vec<u64>, u64) {
    let exe = std::env::current_exe().expect("a path to this program");
    let slots: Vec<_> = (0..cpus)
        .map(|_| {
            let exe = exe.clone();
            thread::spawn(move || {
                let mut counts = vec![0; windows.count];
                let mut spawned = 0;
                while monotonic_ns() < windows.end() {
                    let out = Command::new(&exe)
                        .arg("fair-helper")
                        .args(windows.args())
                        .arg(HELPER_BURST.as_nanos().to_string())
                        .stdin(Stdio::null())
                        .stderr(Stdio::inherit())
                        .output()
                        .expect("a helper spawns");
                    assert!(out.status.success(), "a helper ended {}", out.status);
                    let text = String::from_utf8_lossy(&out.stdout);
                    add(&mut counts, &parse_counts(text.trim(), windows.count).expect("a helper's counts"));
                    spawned += 1;
                }
                (counts, spawned)
            })
        })
        .collect();
    let mut counts = vec![0; windows.count];
    let mut spawned = 0;
    for slot in slots {
        let (c, s) = slot.join().expect("a helper slot panicked");
        add(&mut counts, &c);
        spawned += s;
    }
    (counts, spawned)
}

/// `osbench fair-helper <origin> <width> <count> <burst_ns>`.
pub fn helper(args: &[String]) {
    let windows = Windows::parse(args);
    let burst: u64 = args.get(3).and_then(|a| a.parse().ok()).expect("a burst");
    println!("{}", counts_line(&windows.work(monotonic_ns() + burst)));
}

/// The unit Linux's fair scheduler gave this process a share as: the cgroup
/// whose `cpu` controller governs it, or where none does, its autogroup if
/// autogroup is on.
#[cfg(target_os = "linux")]
fn group_key() -> String {
    let cgroup = crate::sys::cpu_cgroup();
    if cgroup != "/" {
        format!("cgroup:{cgroup}")
    } else if crate::sys::autogroup_enabled() {
        format!("autogroup:{}", crate::sys::autogroup())
    } else {
        "none".into()
    }
}

/// What the jobs' `group=` keys say applied: `3xcgroup` is a cgroup per job.
/// Compared and never printed whole, since a cgroup path names the user.
#[cfg(target_os = "linux")]
fn units(_: Grouping, keys: &[String]) -> String {
    let units: std::collections::BTreeSet<&str> = keys.iter().map(String::as_str).collect();
    let kinds: std::collections::BTreeSet<&str> = units.iter().map(|key| key.split(':').next().unwrap_or(key)).collect();
    format!("{}x{}", units.len(), kinds.into_iter().collect::<Vec<_>>().join("+"))
}

/// A ToyOS process cannot read which job it joined, so the grouping that
/// started them says it.
#[cfg(target_os = "toyos")]
fn units(grouping: Grouping, _: &[String]) -> String {
    match grouping {
        Grouping::Shared => "1xjob",
        Grouping::Apart => "3xjob",
    }
    .into()
}

/// End workloads already started, so none runs into the next measurement.
fn stop(children: impl IntoIterator<Item = (&'static str, std::process::Child)>) {
    for (_, mut child) in children {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// How the coordinator starts each job.
#[derive(Clone, Copy)]
pub enum Grouping {
    /// Every job in this program's group.
    Shared,
    /// Each workload in a group of its own: a ToyOS Job, a Linux scope's cgroup.
    Apart,
    /// Each workload in a Linux session, whose autogroup is the unit.
    #[cfg(target_os = "linux")]
    Session,
}

impl Grouping {
    fn name(self) -> &'static str {
        match self {
            Grouping::Shared => "shared",
            Grouping::Apart => "apart",
            #[cfg(target_os = "linux")]
            Grouping::Session => "session",
        }
    }

    fn command(self, exe: &std::path::Path) -> Result<Command, String> {
        match self {
            Grouping::Shared => Ok(Command::new(exe)),
            #[cfg(target_os = "toyos")]
            Grouping::Apart => {
                use std::os::toyos::process::CommandExt;
                let cap = crate::sys::syscap().ok_or("no capability was endowed, so no job can be made")?;
                let job = cap.new_job().map_err(|e| format!("a new job was refused: {e:?}"))?;
                let mut command = Command::new(exe);
                command.endow(toyos::job::JOB_LABEL, job.into());
                Ok(command)
            }
            #[cfg(target_os = "linux")]
            Grouping::Session => {
                use std::os::unix::process::CommandExt;
                let mut command = Command::new(exe);
                // SAFETY: setsid is async-signal-safe, and a fresh child is
                // no process group leader, so it cannot fail.
                unsafe {
                    command.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    })
                };
                Ok(command)
            }
            #[cfg(target_os = "linux")]
            Grouping::Apart => {
                let mut command = Command::new("systemd-run");
                command.args(["--user", "--scope", "--quiet", "--collect", "-p", "CPUWeight=100", "--"]).arg(exe);
                Ok(command)
            }
        }
    }
}

/// Run the three jobs once under `grouping` and print each one's share.
pub fn contend(grouping: Grouping) {
    let bench = format!("fair.{}", grouping.name());
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return refuse(&bench, format!("no path to this program: {e}")),
    };
    let windows =
        Windows { origin: monotonic_ns() + SETTLE.as_nanos() as u64, width: WINDOW.as_nanos() as u64, count: WINDOWS };
    let mut running = Vec::new();
    for kind in JOBS {
        let mut command = match grouping.command(&exe) {
            Ok(command) => command,
            Err(why) => {
                stop(running);
                return refuse(&bench, format!("job {kind}: {why}"));
            }
        };
        let child = command
            .arg("fair-job")
            .arg(kind)
            .args(windows.args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Ok(child) => running.push((kind, child)),
            Err(e) => {
                stop(running);
                return refuse(&bench, format!("job {kind} would not start: {e}"));
            }
        }
    }
    let mut jobs = Vec::new();
    let mut rest = running.into_iter();
    while let Some((kind, child)) = rest.next() {
        let out = match child.wait_with_output() {
            Ok(out) => out,
            Err(e) => {
                stop(rest);
                return refuse(&bench, format!("job {kind}: {e}"));
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let field = |key: &str| text.split_whitespace().find_map(|w| w.strip_prefix(key)).map(str::to_string);
        let counts = field("counts=").and_then(|c| parse_counts(&c, WINDOWS));
        match (out.status.success(), counts) {
            (true, Some(counts)) => {
                jobs.push((kind, counts, field("helpers=").unwrap_or_default(), field("group=").unwrap_or_default()))
            }
            _ => {
                stop(rest);
                let said = String::from_utf8_lossy(&out.stderr);
                return refuse(
                    &bench,
                    format!("job {kind} ended {}: {}", out.status, said.lines().next().unwrap_or("")),
                );
            }
        }
    }
    let totals: Vec<u64> = (0..WINDOWS).map(|w| jobs.iter().map(|(_, c, _, _)| c[w]).sum()).collect();
    if totals.contains(&0) {
        return refuse(&bench, "a window in which no job finished a chunk");
    }
    let keys: Vec<String> = jobs.iter().map(|(_, _, _, key)| key.clone()).collect();
    condition(&format!("{bench}.units"), units(grouping, &keys));
    let cpus = crate::cpus() as u64;
    for (kind, counts, helpers, _) in &jobs {
        let mut permille: Vec<u64> = counts.iter().zip(&totals).map(|(c, t)| c * 1000 / t).collect();
        permille.sort_unstable();
        let cpus_milli = permille[permille.len() / 2] * cpus;
        let mut extra = vec![("cpus_milli_p50", cpus_milli.to_string())];
        if *kind == "helpers" {
            extra.push(("helpers_spawned", helpers.clone()));
        }
        result(&format!("{bench}.{kind}"), "permille", &mut permille, &extra);
    }
    let mut rate: Vec<u64> = totals.iter().map(|t| t * 1_000_000_000 / windows.width).collect();
    result(&format!("{bench}.total"), "chunks/s", &mut rate, &[]);
}

#[cfg(target_os = "linux")]
pub fn groupings() -> [Grouping; 3] {
    [Grouping::Shared, Grouping::Apart, Grouping::Session]
}

#[cfg(target_os = "toyos")]
pub fn groupings() -> [Grouping; 2] {
    [Grouping::Shared, Grouping::Apart]
}
