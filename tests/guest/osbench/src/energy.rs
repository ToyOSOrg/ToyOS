//! An idle minute's package energy: this program asleep for 60 s and the
//! CPU package's energy counter read each second, mW per second. Linux reads
//! its powercap `package-0` zone, whose counter takes root to read; ToyOS's
//! kernel counts no package energy for a program to read.

const BENCH: &str = "energy.idle_60s.package";

pub fn suite() {
    imp::suite();
}

#[cfg(target_os = "toyos")]
mod imp {
    pub fn suite() {
        crate::report::unsupported(
            super::BENCH,
            "no counter carries the package's energy: toyos_abi::counters::Counter has none",
        );
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::report::{condition, refuse, result, unmeasured};

    const SECONDS: usize = 60;

    fn read_u64(path: &Path) -> std::io::Result<u64> {
        fs::read_to_string(path)?.trim().parse().map_err(std::io::Error::other)
    }

    /// The powercap zone whose name is `package-0`.
    fn zone() -> Option<PathBuf> {
        let mut zones: Vec<PathBuf> = fs::read_dir("/sys/class/powercap").ok()?.flatten().map(|e| e.path()).collect();
        zones.sort();
        zones.into_iter().find(|z| fs::read_to_string(z.join("name")).is_ok_and(|n| n.trim() == "package-0"))
    }

    pub fn suite() {
        let Some(zone) = zone() else {
            return unmeasured(super::BENCH, "no powercap zone is named package-0", &[]);
        };
        let energy = zone.join("energy_uj");
        let range = match read_u64(&zone.join("max_energy_range_uj")) {
            Ok(range) => range,
            Err(e) => return refuse(super::BENCH, format!("max_energy_range_uj: {e}")),
        };
        let mut before = match read_u64(&energy) {
            Ok(uj) => uj,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return unmeasured(super::BENCH, &format!("energy_uj was refused ({e}): `sudo osbench energy` reads it"), &[]);
            }
            Err(e) => return refuse(super::BENCH, format!("energy_uj: {e}")),
        };
        condition("energy.zone", zone.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let start = Instant::now();
        let mut at = start;
        let mut mw = Vec::with_capacity(SECONDS);
        for second in 1..=SECONDS {
            // The minute is the measurement: each read is due a second after the start, not after the last read.
            std::thread::sleep((start + Duration::from_secs(second as u64)).saturating_duration_since(Instant::now()));
            let after = match read_u64(&energy) {
                Ok(uj) => uj,
                Err(e) => return refuse(super::BENCH, format!("energy_uj: {e}")),
            };
            let now = Instant::now();
            let Some(uj) = osbench_rules::energy::span(before, after, range) else {
                return refuse(super::BENCH, format!("energy_uj read {after}, past its range {range}"));
            };
            mw.push(uj * 1000 / u64::try_from((now - at).as_micros()).unwrap_or(u64::MAX).max(1));
            (before, at) = (after, now);
        }
        result(super::BENCH, "mW", &mut mw, &[("seconds", SECONDS.to_string())]);
    }
}
