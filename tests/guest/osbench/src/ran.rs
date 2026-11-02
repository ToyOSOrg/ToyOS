//! Where a row ran. On ToyOS this is read off the kernel's diary for the
//! span between [`start`] and [`Ran::fields`]. Each process the diary saw
//! picked there is listed with the CPU it was picked on most, that CPU's
//! last-level cache and the share of its picks on that CPU. One
//! `ran=name.pid@cpuN/llcL:share%,...` field lists them, most picked first.
//! [`start`] reads the whole diary, so a row opens the span before its
//! warmup, which then rebuilds what the samples run in; the span covers both.
//!
//! Linux reports nothing, and neither does a ToyOS boot that endows no
//! `trace`. A ring that overwrote records inside the span leaves the latest,
//! and `ran_lost` counts the rest. Each CPU has its own ring, so a busy CPU's
//! share is then counted short against a quiet one's.

/// The processes listed, at most.
#[cfg(target_os = "toyos")]
const LISTED: usize = 4;

pub struct Ran {
    #[cfg(target_os = "toyos")]
    diag: Option<crate::sys::diag::Diag>,
}

/// Begin the span: what the diary held before now is read and dropped. A
/// read that fails reports nothing, since the span would include its rest.
/// Diagnostic, measurement branch only: one thread of this process over a
/// span, as [`Seen::thread`] reads it, with the span's lost records.
pub fn thread_over(span: Ran, tid: crate::sys::Tid) -> String {
    #[cfg(target_os = "toyos")]
    {
        let Some(seen) = span.seen() else { return "-".to_string() };
        format!("{}_l{}", seen.thread(std::process::id(), tid as u32), seen.lost())
    }
    #[cfg(not(target_os = "toyos"))]
    {
        let _ = (span, tid);
        "-".to_string()
    }
}

/// Diagnostic, measurement branch only: one pipe round, `mbps~R_<reader>~W_<writer>~o_<others on their cores>~l<lost>`.
pub fn pipe_round(span: Ran, mbps: u64, reader: crate::sys::Tid, writer: crate::sys::Tid) -> String {
    #[cfg(target_os = "toyos")]
    {
        let Some(seen) = span.seen() else { return format!("{mbps}~-") };
        let pid = std::process::id();
        let (r, w) = (reader as u32, writer as u32);
        let homes: Vec<u32> = [seen.home(pid, r), seen.home(pid, w)].into_iter().flatten().collect();
        format!("{mbps}~R_{}~W_{}~o_{}~l{}", seen.thread(pid, r), seen.thread(pid, w), seen.others_on(pid, &homes), seen.lost())
    }
    #[cfg(not(target_os = "toyos"))]
    {
        let _ = (span, reader, writer);
        mbps.to_string()
    }
}

pub fn start() -> Ran {
    #[cfg(target_os = "toyos")]
    {
        let diag = crate::sys::diag::open().filter(|d| d.has_diary());
        let diag = diag.and_then(|mut d| d.diary().2.is_none().then_some(d));
        Ran { diag }
    }
    #[cfg(not(target_os = "toyos"))]
    Ran {}
}

impl Ran {
    /// Diagnostic, measurement branch only: the span's diary, ending it.
    #[cfg(target_os = "toyos")]
    pub fn seen(self) -> Option<Seen> {
        let mut d = self.diag?;
        let (entries, lost, failed) = d.diary();
        if failed.is_some() {
            return None;
        }
        let names = d.roster().into_iter().map(|((pid, _), (name, _))| (pid, name)).collect();
        let apics = d.counters().unwrap_or_default().into_iter().map(|c| (c.cpu, c.apic)).collect();
        Some(Seen { entries: entries.into_iter().map(|(_, e)| e).collect(), lost, names, apics })
    }

    /// The span's fields, ending it.
    pub fn fields(self) -> Vec<(&'static str, String)> {
        #[cfg(target_os = "toyos")]
        {
            let Some(mut d) = self.diag else { return Vec::new() };
            let (entries, lost, failed) = d.diary();
            if failed.is_some() {
                return Vec::new();
            }
            let mut picks: std::collections::BTreeMap<u32, std::collections::BTreeMap<u16, u64>> = Default::default();
            for (_, entry) in &entries {
                if let (toyos_trace::Event::Pick, Some(thread)) = (entry.event, entry.thread) {
                    *picks.entry(thread.pid).or_default().entry(entry.cpu).or_default() += 1;
                }
            }
            let names: std::collections::BTreeMap<u32, String> =
                d.roster().into_iter().map(|((pid, _), (name, _))| (pid, name)).collect();
            let apics: std::collections::BTreeMap<u32, u32> =
                d.counters().unwrap_or_default().into_iter().map(|c| (c.cpu, c.apic)).collect();
            let mut by_total: Vec<(u64, u32, u16, u64)> = picks
                .iter()
                .map(|(&pid, cpus)| {
                    let (&cpu, &most) = cpus.iter().max_by_key(|(_, &n)| n).expect("a listed process was picked");
                    (cpus.values().sum(), pid, cpu, most)
                })
                .collect();
            by_total.sort_by(|a, b| b.0.cmp(&a.0));
            let listed: Vec<String> = by_total
                .iter()
                .take(LISTED)
                .map(|&(total, pid, cpu, most)| {
                    let name = names.get(&pid).map_or_else(|| "?".to_string(), |n| n.replace([' ', ',', '@', ':', '.'], "_"));
                    let llc = apics
                        .get(&u32::from(cpu))
                        .and_then(|&apic| crate::arch::llc_of_apic(apic))
                        .map_or_else(|| "?".to_string(), |l| l.to_string());
                    format!("{name}.{pid}@cpu{cpu}/llc{llc}:{}%", most * 100 / total)
                })
                .collect();
            let mut fields = Vec::new();
            if !listed.is_empty() {
                fields.push(("ran", listed.join(",")));
            }
            if lost > 0 {
                fields.push(("ran_lost", lost.to_string()));
            }
            fields
        }
        #[cfg(not(target_os = "toyos"))]
        Vec::new()
    }
}

/// Diagnostic, measurement branch only: the diary between [`start`] and
/// [`Ran::seen`], read per thread.
#[cfg(target_os = "toyos")]
pub struct Seen {
    entries: Vec<toyos_trace::Entry>,
    lost: u64,
    names: std::collections::BTreeMap<u32, String>,
    apics: std::collections::BTreeMap<u32, u32>,
}

#[cfg(target_os = "toyos")]
impl Seen {
    fn llc(&self, cpu: u32) -> Option<u32> {
        self.apics.get(&cpu).and_then(|&apic| crate::arch::llc_of_apic(apic))
    }

    /// One thread: the CPUs it was picked on as `cpu*picks`, most first; its
    /// migrations, all and across an LLC; and its wakes by their waker's CPU
    /// (the record's data, the waker plus one, which only a kernel naming
    /// wakers writes): none named, in the LLC that took the wake, in another.
    /// `c10*40+2*3_m1-1_w0-5-2`.
    pub fn thread(&self, pid: u32, tid: u32) -> String {
        let mut picks: std::collections::BTreeMap<u16, u64> = Default::default();
        let (mut moved, mut crossed, mut none, mut near, mut far) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for entry in &self.entries {
            if entry.thread != Some(toyos_trace::Thread { pid, tid }) {
                continue;
            }
            match entry.event {
                toyos_trace::Event::Pick => *picks.entry(entry.cpu).or_default() += 1,
                toyos_trace::Event::Migrate { to } => {
                    moved += 1;
                    crossed += u64::from(self.llc(u32::from(entry.cpu)) != self.llc(to));
                }
                toyos_trace::Event::Wake { waker: 0 } => none += 1,
                toyos_trace::Event::Wake { waker } => {
                    if self.llc(waker - 1) == self.llc(u32::from(entry.cpu)) {
                        near += 1;
                    } else {
                        far += 1;
                    }
                }
                _ => {}
            }
        }
        let mut cpus: Vec<(u16, u64)> = picks.into_iter().collect();
        cpus.sort_by(|a, b| b.1.cmp(&a.1));
        let cpus: Vec<String> = cpus.iter().map(|(cpu, n)| format!("{cpu}*{n}")).collect();
        format!("c{}_m{moved}-{crossed}_w{none}-{near}-{far}", cpus.join("+"))
    }

    /// The CPU `pid`'s `tid` was picked on most, if it was.
    pub fn home(&self, pid: u32, tid: u32) -> Option<u32> {
        let mut picks: std::collections::BTreeMap<u16, u64> = Default::default();
        for entry in &self.entries {
            if entry.thread == Some(toyos_trace::Thread { pid, tid }) && entry.event == toyos_trace::Event::Pick {
                *picks.entry(entry.cpu).or_default() += 1;
            }
        }
        picks.into_iter().max_by_key(|&(_, n)| n).map(|(cpu, _)| u32::from(cpu))
    }

    /// Every other process picked on a CPU of the cores `cpus` are on, as
    /// `name.pid@cpu*picks`, most first, at most [`LISTED`]: `-` for none.
    pub fn others_on(&self, pid: u32, cpus: &[u32]) -> String {
        let cores: Vec<u32> = cpus.iter().map(|&cpu| crate::sys::core_of(cpu)).collect();
        let mut picks: std::collections::BTreeMap<(u32, u16), u64> = Default::default();
        for entry in &self.entries {
            if let (toyos_trace::Event::Pick, Some(thread)) = (entry.event, entry.thread) {
                if thread.pid != pid && cores.contains(&crate::sys::core_of(u32::from(entry.cpu))) {
                    *picks.entry((thread.pid, entry.cpu)).or_default() += 1;
                }
            }
        }
        let mut listed: Vec<((u32, u16), u64)> = picks.into_iter().collect();
        listed.sort_by(|a, b| b.1.cmp(&a.1));
        let listed: Vec<String> = listed
            .iter()
            .take(LISTED)
            .map(|&((other, cpu), n)| {
                let name = self.names.get(&other).map_or_else(|| "?".to_string(), |n| n.replace([' ', ',', '@', ':', '.', '_', '~', '+', '*'], "-"));
                format!("{name}.{other}@{cpu}*{n}")
            })
            .collect();
        if listed.is_empty() { "-".to_string() } else { listed.join("+") }
    }

    pub fn lost(&self) -> u64 {
        self.lost
    }
}
