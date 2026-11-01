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
