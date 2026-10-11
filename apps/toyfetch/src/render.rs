//! What toyfetch prints: the system's logo on the left, if it has one, and a
//! `label: value` line per fact it answered on the right, in ANSI SGR colour.

/// What the system said of itself. A fact it did not answer is `None` and
/// prints no line.
pub struct Facts {
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: String,
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    /// Seconds since boot.
    pub uptime: Option<u64>,
    pub shell: Option<String>,
    pub terminal: Option<String>,
}

pub struct Cpu {
    /// `None` where the system names no model.
    pub brand: Option<String>,
    pub count: usize,
}

/// Bytes.
pub struct Memory {
    pub used: u64,
    pub total: u64,
}

/// A logo: its lines, each with the SGR foreground colour it is drawn in.
pub struct Logo {
    pub lines: &'static [(u8, &'static str)],
}

/// A spinning top.
const TOYOS: Logo = Logo {
    lines: &[
        (37, "       _|_"),
        (36, "    .-'   '-."),
        (36, "  .'         '."),
        (33, " (====ToyOS====)"),
        (35, "  '.         .'"),
        (35, "    '.     .'"),
        (35, "      '. .'"),
        (37, "        V"),
    ],
};

/// The logo of the system `os-release(5)`'s `ID` names, if toyfetch has one.
pub fn logo_for(distribution_id: &str) -> Option<&'static Logo> {
    match distribution_id {
        "toyos" => Some(&TOYOS),
        _ => None,
    }
}

/// The SGR foreground colour every label takes.
const ACCENT: u8 = 36;
const RESET: &str = "\x1b[0m";
/// Columns between the logo and the facts.
const GAP: usize = 3;
const MIB: u64 = 1024 * 1024;

pub fn render(facts: &Facts, logo: Option<&Logo>) -> String {
    let mut info: Vec<String> = fields(facts)
        .into_iter()
        .filter_map(|(label, value)| Some(format!("\x1b[1;{ACCENT}m{label}{RESET}: {}", value?)))
        .collect();
    info.push(String::new());
    info.push(
        (0..8)
            .map(|colour| format!("\x1b[4{colour}m   "))
            .collect::<String>()
            + RESET,
    );

    let logo_lines = logo.map_or(&[][..], |logo| logo.lines);
    let width = logo_lines
        .iter()
        .map(|(_, line)| line.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    for row in 0..logo_lines.len().max(info.len()) {
        let fact = info.get(row).map_or("", String::as_str);
        if let Some(logo) = logo {
            let (colour, line) = logo.lines.get(row).copied().unwrap_or((0, ""));
            if !line.is_empty() {
                out.push_str(&format!("\x1b[1;{colour}m{line}{RESET}"));
            }
            if !fact.is_empty() {
                out.push_str(&" ".repeat(width - line.chars().count() + GAP));
            }
        }
        out.push_str(fact);
        out.push('\n');
    }
    out
}

fn fields(facts: &Facts) -> [(&'static str, Option<String>); 8] {
    [
        ("OS", facts.os.clone()),
        ("Kernel", facts.kernel.clone()),
        ("Arch", Some(facts.arch.clone())),
        (
            "CPU",
            facts.cpu.as_ref().map(|cpu| match &cpu.brand {
                Some(brand) => format!("{brand} ({})", cpu.count),
                None => format!("model not reported ({})", cpu.count),
            }),
        ),
        (
            "Memory",
            facts
                .memory
                .as_ref()
                .map(|m| format!("{} MiB / {} MiB", m.used / MIB, m.total / MIB)),
        ),
        ("Uptime", facts.uptime.map(uptime)),
        ("Shell", facts.shell.clone()),
        ("Terminal", facts.terminal.clone()),
    ]
}

/// `secs` as days, hours, minutes and seconds, leaving out each unit that is
/// zero.
fn uptime(secs: u64) -> String {
    let units = [
        (secs / 86_400, "d"),
        (secs / 3_600 % 24, "h"),
        (secs / 60 % 60, "m"),
        (secs % 60, "s"),
    ];
    let said: Vec<String> = units
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect();
    if said.is_empty() {
        "0s".to_owned()
    } else {
        said.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWATCH: &str = "\x1b[40m   \x1b[41m   \x1b[42m   \x1b[43m   \x1b[44m   \x1b[45m   \x1b[46m   \x1b[47m   \x1b[0m";

    fn full() -> Facts {
        Facts {
            os: Some("ToyOS 1a2b3c4d5e6f (dirty)".into()),
            kernel: Some("ToyOS 1a2b3c4d5e6f-dirty".into()),
            arch: "x86_64".into(),
            cpu: Some(Cpu {
                brand: Some("AMD Ryzen 7 PRO 4750U".into()),
                count: 16,
            }),
            memory: Some(Memory {
                used: 300 * MIB + 5,
                total: 2048 * MIB,
            }),
            uptime: Some(90_061),
            shell: Some("zsh".into()),
            terminal: Some("Apple_Terminal".into()),
        }
    }

    fn bare() -> Facts {
        Facts {
            os: None,
            kernel: None,
            arch: "aarch64".into(),
            cpu: None,
            memory: None,
            uptime: None,
            shell: None,
            terminal: None,
        }
    }

    /// The text a terminal shows: every SGR sequence taken out.
    fn shown(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("\x1b[") {
            out.push_str(&rest[..at]);
            let end = rest[at..].find('m').expect("an SGR sequence ends in m");
            rest = &rest[at + end + 1..];
        }
        out + rest
    }

    #[test]
    fn every_fact_prints_beside_the_logo_in_its_colours() {
        assert_eq!(
            render(&full(), Some(&TOYOS)),
            "\x1b[1;37m       _|_\x1b[0m         \x1b[1;36mOS\x1b[0m: ToyOS 1a2b3c4d5e6f (dirty)\n\
             \x1b[1;36m    .-'   '-.\x1b[0m      \x1b[1;36mKernel\x1b[0m: ToyOS 1a2b3c4d5e6f-dirty\n\
             \x1b[1;36m  .'         '.\x1b[0m    \x1b[1;36mArch\x1b[0m: x86_64\n\
             \x1b[1;33m (====ToyOS====)\x1b[0m   \x1b[1;36mCPU\x1b[0m: AMD Ryzen 7 PRO 4750U (16)\n\
             \x1b[1;35m  '.         .'\x1b[0m    \x1b[1;36mMemory\x1b[0m: 300 MiB / 2048 MiB\n\
             \x1b[1;35m    '.     .'\x1b[0m      \x1b[1;36mUptime\x1b[0m: 1d 1h 1m 1s\n\
             \x1b[1;35m      '. .'\x1b[0m        \x1b[1;36mShell\x1b[0m: zsh\n\
             \x1b[1;37m        V\x1b[0m          \x1b[1;36mTerminal\x1b[0m: Apple_Terminal\n\
             \n"
            .to_owned()
                + "                   "
                + SWATCH
                + "\n"
        );
    }

    #[test]
    fn a_fact_the_system_did_not_answer_prints_no_line() {
        assert_eq!(
            shown(&render(&bare(), None)),
            "Arch: aarch64\n\n                        \n"
        );
        let said = shown(&render(&bare(), Some(&TOYOS)));
        for label in [
            "OS", "Kernel", "CPU", "Memory", "Uptime", "Shell", "Terminal",
        ] {
            assert!(!said.contains(&format!("{label}:")), "{label} in\n{said}");
        }
    }

    /// The logo is taller than three facts: its rows below them carry no
    /// padding, and every fact starts in one column.
    #[test]
    fn the_logo_taller_than_the_facts_pads_only_rows_with_a_fact() {
        let said = shown(&render(&bare(), Some(&TOYOS)));
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines.len(), TOYOS.lines.len());
        assert_eq!(lines[0], "       _|_         Arch: aarch64");
        assert_eq!(lines[1], "    .-'   '-.");
        assert_eq!(lines[2], format!("  .'         '.    {}", " ".repeat(24)));
        assert_eq!(lines[3], " (====ToyOS====)");
    }

    /// The facts are taller than the logo: their rows below it start in the
    /// column the logo's rows put them in.
    #[test]
    fn the_facts_taller_than_the_logo_keep_their_column() {
        const SHORT: Logo = Logo {
            lines: &[(31, "/\\"), (31, "\\/")],
        };
        let said = shown(&render(&full(), Some(&SHORT)));
        let lines: Vec<&str> = said.lines().collect();
        assert_eq!(lines[0], "/\\   OS: ToyOS 1a2b3c4d5e6f (dirty)");
        assert_eq!(lines[1], "\\/   Kernel: ToyOS 1a2b3c4d5e6f-dirty");
        assert_eq!(lines[2], "     Arch: x86_64");
        assert_eq!(lines[7], "     Terminal: Apple_Terminal");
        assert_eq!(lines[8], "");
    }

    #[test]
    fn a_cpu_without_a_model_says_so() {
        let facts = Facts {
            cpu: Some(Cpu {
                brand: None,
                count: 4,
            }),
            ..bare()
        };
        assert!(shown(&render(&facts, None)).contains("CPU: model not reported (4)\n"));
    }

    #[test]
    fn only_toyos_has_a_logo() {
        assert!(logo_for("toyos").is_some());
        for other in ["macos", "ubuntu", "windows", "ToyOS", ""] {
            assert!(logo_for(other).is_none(), "{other}");
        }
    }

    #[test]
    fn uptime_leaves_out_zero_units() {
        assert_eq!(uptime(0), "0s");
        assert_eq!(uptime(59), "59s");
        assert_eq!(uptime(3_600), "1h");
        assert_eq!(uptime(86_400 + 120), "1d 2m");
        assert_eq!(uptime(90_061), "1d 1h 1m 1s");
    }
}
