//! The double fault path, which is the one that has to survive being the
//! thing that reports on itself.
//!
//! #DF is the only vector with an IST, so it is the only stack in the kernel
//! whose overflow is invisible: it is heap memory, it is written while the
//! crash report is being produced, and the corruption lands under whatever
//! the allocator handed out next. A test that only asserted "the report
//! appeared" would have passed throughout -- the report *did* appear, and it
//! scribbled on the heap on its way out.
//!
//! So the assertion is the kernel's own high-water measurement, taken after
//! `panic_flush` (the deepest point) and written straight to the UART rather
//! than through the log ring, which is one of the things an overflow may have
//! corrupted.
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial::Serial;

/// The line `ist1_report` writes to the UART.
const MARKER: &str = "[ist1] used ";

pub fn double_fault_stack(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    // Profile::Metal, because there the 16550 *is* the console, so the raw
    // write and the ordinary serial stream arrive on the same channel and one
    // reader sees both. It is also the T14's shape, which is the machine this
    // bug would have poisoned every double-fault investigation on.
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            kernel_features: toyos_build::build::TEST_KERNEL,
            ..Default::default()
        },
    );

    writeln!(qemu.stdin_mut(), "run test_rs_test_panic_child 4").expect("write to QEMU stdin");
    qemu.flush_stdin();
    // Until the report, not for twenty seconds: the fatal path halts every CPU
    // without exiting QEMU, so a plain drain has nothing left to disconnect it
    // and waits out the whole ceiling. The marker is the line every assertion
    // below reads, and `ist1_report` writes it last.
    let log = qemu.drain_until(Duration::from_secs(20), |line| line.contains(MARKER));

    // The premise. If the CPU never took a #DF then nothing ran on IST1 and
    // every assertion below would be measuring the wrong stack.
    if !log.contains("DOUBLE FAULT") {
        return Err(format!("no double fault was taken — the trigger did not work\n{log}"));
    }

    // **And the harness's own claim about a capture like this one, asked of the
    // only real one the suite produces.** `serial::death_report` is what a
    // failure verdict now carries, and it is staged against transcribed lines
    // everywhere else; a #DF is on the wire here already, so checking it costs
    // nothing and is the difference between a recovery gated on a guess about
    // the kernel's output and one gated on the output. It is a claim about the
    // report and not about IST1, which is why it sits above every assertion
    // that is.
    let report = super::serial::death_report(&log).ok_or_else(|| {
        format!("a capture carrying a real #DF yields no death report at all\n{log}")
    })?;
    let head = report.lines().next().unwrap_or_default();
    if !head.contains("DOUBLE FAULT") {
        return Err(format!("the report starts at {head:?} and not at the death\n{log}"));
    }
    // The body. The header alone is what the arm that lost this report already
    // printed, so the assertion is on the lines under it: the address that
    // started the chain, and the backtrace `double_fault_handler` writes after
    // the page walk.
    for want in ["cr2=", "Kernel backtrace:", MARKER] {
        if !report.contains(want) {
            return Err(format!("the report drops {want:?}:\n{report}"));
        }
    }
    let Some(line) = log.lines().find(|l| l.contains(MARKER)) else {
        return Err(format!(
            "the kernel never reported its IST1 usage; the report cannot have run to the \
             end on IST1\n{log}"
        ));
    };

    let (used, capacity) = parse(line)
        .ok_or_else(|| format!("could not read a usage out of {line:?}"))?;
    eprintln!("  [ist1] double fault report used {used} of {capacity} bytes");

    if line.contains("GUARD CORRUPTED") {
        return Err(format!(
            "the double fault report overflowed IST1 and wrote into the heap below it: \
             {used} bytes used of {capacity}"
        ));
    }
    if !line.contains("guard intact") {
        return Err(format!("unrecognised verdict in {line:?}"));
    }
    // Not just "it fit": it has to fit with room, or the next line added to
    // the crash report silently reintroduces the bug. Half the stack is the
    // margin, and it is stated here so that a change which eats it fails
    // here rather than on somebody's laptop.
    if used * 2 > capacity {
        return Err(format!(
            "the double fault report used {used} of {capacity} bytes — over half the stack, \
             so the margin for one more report line is gone"
        ));
    }
    Ok(())
}

/// The guard page under every per-CPU idle stack.
///
/// That stack is 16 KiB of ordinary heap, so an overflow off its bottom did
/// not fault — it rewrote whatever the allocator had put underneath, and the
/// damage surfaced somewhere else entirely (a `BTreeMap` node with an
/// out-of-range index, a write to `0x4`). The idle loop ran `log_file::poll`
/// when that was measured — a filesystem write reaching a block device, whose
/// high water was 11,505 bytes of the 16,384 with the USB command path still
/// below the probe. That caller is gone at log architecture L6 and `drain_irqs`
/// still reaches a device from the same stack.
///
/// Absence is invisible to every log line and every screendump, so the only
/// way to ask whether the page is really gone is to touch it — which nothing
/// in the kernel does, that being the point of a guard page. `SYS_DEBUG` action
/// 9 supplies the one read.
pub fn idle_stack_guard(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            kernel_features: toyos_build::build::TEST_KERNEL,
            ..Default::default()
        },
    );

    writeln!(qemu.stdin_mut(), "run test_rs_test_panic_child 9").expect("write to QEMU stdin");
    qemu.flush_stdin();
    // Until the page walk, not for twenty seconds — `double_fault_stack`'s
    // shape and for its reason: this fault is fatal, so `halt_all_cpus` stops
    // every CPU without QEMU exiting and a plain drain has nothing left to
    // disconnect it. `debug_page_walk` is the last thing any assertion below
    // reads (PDPTE, then the PDE carrying `PS=`, then this line), and it runs
    // early in the crash report, so what follows on the wire — registers,
    // backtrace, stack — is diagnostic that nothing here asks for. A boot where
    // the guard is *not* there prints no page walk at all, which is the
    // `debug syscall returned` arm below: it pays the whole ceiling and then
    // reds, which is the right way round.
    //
    // **The three spaces are load-bearing.** `PDPTE:` one level up contains
    // `PTE:` as a substring, so the obvious predicate ends the drain two lines
    // early and reds a green machine with `the crash report's page walk does
    // not show a split leaf` — measured, on this change's first run.
    // `mm::paging::debug_page_walk` writes `PTE:   {:#018x}`, which is the
    // spelling the assertion below reads too.
    let log = qemu.drain_until(Duration::from_secs(20), |line| line.contains("PTE:   0x"));

    // The premise: which address the kernel went for. Without it every
    // assertion below could be satisfied by a fault somewhere else.
    let addr = log
        .lines()
        .find_map(|l| l.split("reading the idle stack guard at ").nth(1))
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .ok_or_else(|| {
            format!("the kernel never reached the guard read — is `test-actuators` on?\n{log}")
        })?;

    // The tell of a guard that is not there: `SYS_DEBUG` returned, so the read
    // landed on dlmalloc's bookkeeping for the chunk the idle stack lives in
    // and the child walked away.
    if log.contains("debug syscall returned") {
        return Err(format!(
            "the read at {addr} succeeded — the page below the idle stack is still mapped, \
             so an overflow writes into the heap instead of faulting"
        ));
    }
    for want in [
        format!("#PF UNHANDLED: cr2={addr}"),
        format!("KERNEL PANIC: read unmapped address at {addr}"),
    ] {
        if !log.contains(&want) {
            return Err(format!("no {want:?}; the kernel said:\n{log}"));
        }
    }
    // The page walk is the ground truth, and it is in the report: a PDE that
    // is a page table rather than a 2 MiB leaf, and a PTE of zero under it.
    // Without the split the direct map would still show `PS=1` here.
    if !log.contains("PS=0") || !log.contains("PTE:   0x0000000000000000") {
        return Err(format!(
            "the crash report's page walk does not show a split leaf with an empty entry:\n{log}"
        ));
    }
    eprintln!("  [guard] a read at {addr} faulted, one page below the idle stack");

    // And the machine halts, which is the intended end. An overflow off the
    // bottom of the idle stack is a kernel bug, not untrusted input, and
    // `fatal_exception` treats a fault on a *kernel* address as fatal by
    // policy. The whole change is that it is now reported at all: without the
    // guard the same overflow writes into the heap and the machine carries on
    // with a `BTreeMap` node the allocator no longer agrees about.
    Ok(())
}

/// A NIC that cannot raise an interrupt must cost the machine networking and
/// nothing else.
///
/// The other two virtio functions keep their vectors, which is what makes the
/// verdict mean anything: the console that carries the refusal and the audio
/// device beside it are on the same bus, driven by the same code, and neither
/// notices.
pub fn virtio_net_no_msix() -> Result<(), String> {
    let options = BootOptions {
        profile: qemu::Profile::VirtioNetNoMsix,
        ..Default::default()
    };
    // The actuator is a device property and argv is the only place one is
    // visible: a NIC that quietly kept its MSI-X table would make every line
    // below a re-run of the happy path under a different name.
    let argv = qemu::profile_argv(&options);
    let devices = |kind: &str| -> Vec<&str> {
        argv.windows(2)
            .filter(|w| w[0] == "-device" && w[1].starts_with(kind))
            .map(|w| w[1].as_str())
            .collect()
    };
    let nics = devices("virtio-net");
    let [nic] = nics[..] else {
        return Err(format!("this profile is one NIC; argv has {nics:?}"));
    };
    if !nic.contains("vectors=0") {
        return Err(format!("{nic} still has its MSI-X table"));
    }
    for kind in ["virtio-sound", "virtio-serial"] {
        let others = devices(kind);
        let [other] = others[..] else {
            return Err(format!("this profile is one {kind}; argv has {others:?}"));
        };
        if other.contains("vectors=") {
            return Err(format!(
                "{other} is crippled too, so a refusal could not be shown to be per device \
                 — and with no console there would be nothing to read it on"
            ));
        }
    }

    // `tests/netcase` rather than the ordinary config, because it is the one
    // that runs netd — and netd's own answer is the assertion below that the
    // refusal reached userland rather than stopping at a log line.
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
    let (log, exited) = netd_answered(QemuInstance::boot_with_options(&config, &[], &[], options));

    // Refused by name, at a named function, and not by claiming a mode it does
    // not have: the xHCI driver's `polled mode` line is the defect this whole
    // family exists to keep out of the tree.
    refused_claim(
        &log,
        super::https::VIRTIO.claims,
        "neither its MSI-X nor its MSI could be armed",
        &[],
    )?;
    // And it reached userland rather than stopping at a log line.
    exited?;
    // And the machine is otherwise whole. `must_be_clean` is what makes the
    // change from `panic!` an assertion rather than a hope.
    log.must_say("virtio-sound: MSI-X vector")?;
    log.must_say("Boot: complete")?;
    log.must_be_clean()?;
    Ok(())
}

/// A claimed function whose capability list ends at a link the spec forbids is
/// refused, and never armed on the older mechanism the walk did reach.
///
/// No device in reach publishes that shape, so the actuator stages it — for
/// this claim's own walks and nothing else.
pub fn claim_caps_truncated() -> Result<(), String> {
    // The bench whose claimed function publishes MSI as well: on one that
    // publishes neither mechanism the refusal is the one `virtio_net_no_msix`
    // already earns, and no table BAR is at stake.
    let bench = super::https::E1000E;
    let options = BootOptions {
        profile: bench.profile,
        kernel_params: &["pcidev-caps-truncated"],
        ..Default::default()
    };
    let config = super::compile::repo_root().join(bench.config);
    let (log, exited) = netd_answered(QemuInstance::boot_with_options(&config, &[], &[], options));

    // Refused by the reason that is true of it: what the list holds past that
    // link was never read — not "it has no table".
    refused_claim(&log, bench.claims, "its capability list ends at a link the PCI spec forbids", &[])?;
    // And it reached userland rather than stopping at a log line.
    exited?;
    // And the machine is otherwise whole: one claim refused costs networking
    // and nothing else.
    log.must_say("Boot: complete")?;
    log.must_be_clean()?;
    Ok(())
}

/// The slot QEMU's `-device` order puts the function netd claims on, and the
/// address every judge below is an assertion about.
///
/// **The address is the harness's own and never the guest's.** A judge that
/// reads the function out of the console and then asserts about *that* asserts
/// about whichever function the kernel happened to name; what the guest printed
/// is asserted equal to this instead, so a constant that names the wrong slot
/// reds and never passes.
pub const CLAIMED_AT: &str = "00:03.0";

/// The two lines a hand-over of that function spends. One arm requires them and
/// [`refused_claim`] requires their absence, and both read them here: a kernel
/// that stopped writing either line would otherwise satisfy both.
pub fn bar_moved() -> String {
    format!("pcidev: PCI {CLAIMED_AT} BAR")
}

pub fn msix_armed() -> String {
    format!("PCI {CLAIMED_AT}: msix address=")
}

/// The older mechanism taken where the newer one was published — required
/// absent by [`refused_claim`] and by [`super::iommu::armed_on_msix`], and read
/// here by both for [`msix_armed`]'s reason.
pub fn msi_armed() -> String {
    format!("PCI {CLAIMED_AT}: msi address=")
}

/// Every function named by a line carrying `marker`, in the kernel's own
/// spelling.
///
/// **A line that carries the marker and no `pcidev: PCI ` prefix is an error,
/// never a dropped line.** A scan closes only the spellings it matches, so a
/// caller asking what a console named on *every* such line would otherwise be
/// answered about the subset this walk could parse — one refusal read and a
/// second one dropped is the case "and no other function" exists for.
pub fn functions_named<'a>(log: &'a Serial, marker: &str) -> Result<Vec<&'a str>, String> {
    const PREFIX: &str = "pcidev: PCI ";
    let mut named = Vec::new();
    for line in log.text().lines().filter(|line| line.contains(marker)) {
        named.push(
            line.split(PREFIX)
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .ok_or_else(|| {
                    format!("{line:?} says {marker:?} and names no function after {PREFIX:?}")
                })?,
        );
    }
    Ok(named)
}

/// netd's own answer on a machine it was given no NIC on.
const NETD_EXITS: &str = "netd: no NIC on this machine, exiting";

/// Wait for that answer, and hand back the boot console beside it.
///
/// netd is spawned before the ready marker and speaks after it, so its line is
/// drained for rather than read out of the boot capture. **What is waited for
/// is the whole line and not a prefix naming the program**: init reports the
/// claim it could not make as `init: netd: ...`, and that is already in the
/// boot capture before netd has run at all, so a `"netd: "` predicate is
/// satisfied by the wrong speaker. netd announces itself instead when the claim
/// was *not* refused, so a kernel that handed the function over ends the wait
/// at once rather than being waited out to the stall budget.
///
/// The verdict is handed back rather than raised, so a caller judges the
/// kernel's own half first: a kernel that handed the function over fails on
/// what it printed about the function, not on what netd did about it.
fn netd_answered(mut qemu: QemuInstance) -> (Serial, Result<(), String>) {
    const NETD_RUNS: &str = "netd: ready, at most ";
    let mut text = qemu.boot_log().to_string();
    let stalled = qemu::await_guest(&mut qemu, &mut text, "netd's own answer", |c| {
        c.contains(NETD_EXITS) || c.contains(NETD_RUNS)
    })
    .err();
    let log = Serial::named("boot console", text);
    let exited = if log.text().contains(NETD_EXITS) {
        Ok(())
    } else {
        Err(format!(
            "{}{NETD_EXITS:?} never reached the boot console:\n{}",
            stalled.map(|why| format!("{why}\n")).unwrap_or_default(),
            log.text()
        ))
    };
    (log, exited)
}

/// **The claim on [`CLAIMED_AT`] was refused for `why`, and the refusal spent
/// nothing**: no BAR of that function moved, neither of its two message
/// mechanisms is armed, `claims` reached no holder, and init said so in the
/// boot config's own spelling. `beside` is every other function this machine
/// refuses, each judged by its own caller.
///
/// The three arms that refuse a claim read this one judge, so a kernel that
/// answered a refusal by logging it and handing the function over anyway is red
/// wherever the refusal is reached. `slot_space` put back below `place_bars`
/// reds on the two unspent lines.
pub fn refused_claim(log: &Serial, claims: &str, why: &str, beside: &[&str]) -> Result<(), String> {
    let refused = functions_named(log, "NOT HANDED OVER")?;
    let others: std::collections::BTreeSet<&str> = refused.iter().copied().filter(|at| *at != CLAIMED_AT).collect();
    if !refused.contains(&CLAIMED_AT) || others != beside.iter().copied().collect() {
        return Err(format!(
            "the claim this judges is the one on {CLAIMED_AT}, beside {beside:?}; this console \
             refused {refused:?}:\n{}",
            log.text()
        ));
    }
    // By the reason true of the path that raised it, on the line that names the
    // function: a refusal whose reason belongs to another path is worse than no
    // line at all.
    log.must_say(&format!("pcidev: PCI {CLAIMED_AT} NOT HANDED OVER — {why}"))?;
    log.must_not_say(&format!("[{claims}] handed over"))?;
    log.must_not_say(&msix_armed())?;
    log.must_not_say(&msi_armed())?;
    log.must_not_say(&bar_moved())?;
    // All the way out to userland, rather than a kernel that logged a refusal
    // and handed netd a NIC anyway. init names what it could not mint in the
    // config's own spelling, and **with this refusal's own word**: the machine
    // has the function, so "no such device on this machine" would be false.
    log.must_say(&format!(
        "init: netd: pci:{claims} is on this machine and could not be handed over"
    ))?;
    Ok(())
}

/// A machine with no NVMe controller must boot, and its block service and
/// file servers serve what they have: absence of storage is a configuration,
/// not a failure.
pub fn diskless_boot(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions {
        profile: qemu::Profile::Diskless,
        ..Default::default()
    };
    // The teeth, and the only ones: absence is invisible to every console line
    // and every screendump, so the argv is where it has to be checked. Only
    // the value following `-device`/`-drive` is a device claim — every other
    // element, including four filesystem paths, is not one, and a worktree
    // checked out under a path containing "nvme" made a plain substring scan
    // over the whole argv false-positive on itself.
    let argv = qemu::profile_argv(&options);
    if argv.windows(2).any(|w| (w[0] == "-device" || w[0] == "-drive") && w[1].contains("nvme")) {
        return Err(format!("the diskless profile still has an NVMe device: {argv:?}"));
    }

    let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let log = crate::common::serial::Serial::boot(&qemu);

    // The two absence claims are only claims if the console carried anything,
    // and `must_not_say` is what establishes that. The positives below made
    // this safe by luck rather than by design -- reorder them and the panic
    // scan is a claim about nothing again.
    log.must_be_clean()?;
    log.must_not_say("no controller found")?;
    log.must_say("blockd: no NVMe controller this row names is on this machine; serving no partition")?;
    log.must_say("fsd: this machine has no DATA partition;")?;
    log.must_say("Boot: complete")?;
    Ok(())
}

/// How long the guest spins. The storm arms about 190 ms after the spinner
/// starts — a million syscalls at its measured rate — and this is what covers a
/// slow arming plus the storm itself on a shard with company.
const SPIN_SECS: u32 = 10;

/// The negative control on the NMI storm `syscall-window-nmi` arms: an NMI
/// handler that returns early through `iretq` un-masks NMIs while still standing
/// on IST2, which is the one way a second NMI can enter on that stack. The check
/// has to fire and say so.
pub fn syscall_window_nmi_controls(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let nested = storm(
        test_config,
        c_bins,
        rust_bins,
        &["syscall-window-nmi", "nmi-nested"],
        SPIN_SECS,
        |l| l.contains("NESTED NMI"),
    )?;
    let Some(loud) = nested.lines().find(|l| l.contains("NESTED NMI")) else {
        return Err(format!(
            "a second NMI entered on IST2 and the machine said nothing: the outer handler's \
             frame was overwritten silently, which is the failure this check exists for\n{nested}"
        ));
    };
    eprintln!("  [nmi-window] nested: {}", loud.trim());
    Ok(())
}

/// One storm boot: the spinner in Ring 3, the kernel's NMIs at it, drained until
/// `done` or the ceiling.
///
/// The ceiling is a ceiling and not the run — every arm here ends either with
/// the kernel's report or with a halted machine, and a halted machine neither
/// exits QEMU nor disconnects the drain.
fn storm(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    params: &'static [&'static str],
    secs: u32,
    done: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let options = BootOptions {
        kernel_params: params,
        // `double_fault_stack`'s profile and for its reason: on Metal the 16550
        // *is* the console, so `serial::panic_raw`'s bytes and the ordinary log
        // stream arrive on one channel and one reader sees both. The nested-NMI
        // report is a raw write — that handler may not reach the log ring at all
        // (`arch::idt::nmi`) — so on any other profile it lands on a UART
        // nothing here is reading.
        profile: qemu::Profile::Metal,
        // Four, so that the scheduler has somewhere to put the spinner that is
        // not the CPU whose idle loop does the storming.
        smp: 4,
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    writeln!(qemu.stdin_mut(), "run test_rs_nmi_window_spin {secs}").expect("write to QEMU stdin");
    qemu.flush_stdin();
    Ok(qemu.drain_until(Duration::from_secs(u64::from(secs) + 20), |line| done(line)))
}

/// `[ist1] used N of M bytes, ...`
fn parse(line: &str) -> Option<(usize, usize)> {
    let rest = line.split(MARKER).nth(1)?;
    let mut words = rest.split_whitespace();
    let used = words.next()?.parse().ok()?;
    if words.next()? != "of" {
        return None;
    }
    let capacity = words.next()?.parse().ok()?;
    Some((used, capacity))
}

/// The blocked-task dump's NMI probe: a CPU that ignores a kick is named, and
/// then asked where it is with the one interrupt it cannot mask.
///
/// The verdict `no answer: it did not reach a scheduler pass` has three causes
/// — spinning with `IF` clear, halted with a lost kick, wedged below the
/// interrupt layer — and on the owner's T14 it named three CPUs without saying
/// which. The NMI separates them, so what this asserts is the separation: the
/// kick goes unanswered, the NMI is answered, and the `rip` it brings back
/// lands in the spin the actuator is executing.
///
/// The last assertion is the one that keeps the instrument honest. A probe that
/// reported *some* address would satisfy every other line here; only resolving
/// it against the kernel's own symbols says the report points at where the CPU
/// actually was.
///
/// **Judged on the T14 and in no QEMU guest.** The deafness is a window of the
/// actuator's own clock and the dump's kick and NMI budgets are the kernel's,
/// so a guest the host starves misses the window and reads exactly like the
/// defect this hunts.
pub fn dump_nmi_probe_on_metal(kernel: &Serial) -> Result<(), String> {
    let log = kernel.text();
    if !log.contains("=== blocked-task dump:") {
        return Err(format!("the dump never ran — is `dump-deaf-cpu` on?\n{log}"));
    }
    let silent: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("no answer: it did not reach a scheduler pass"))
        .collect();
    if silent.len() != 1 {
        return Err(format!(
            "expected exactly the deafened CPU to miss its kick, got {}:\n{}\n{log}",
            silent.len(),
            silent.join("\n"),
        ));
    }
    if log.contains("no NMI answer either") {
        return Err(format!(
            "the NMI went unanswered too. The victim spins with IF clear and an NMI is not \
             maskable by IF, so this says the NMI never reached it at all — vector 2, the ICR \
             delivery mode, or the handler.\n{log}"
        ));
    }
    let Some(rest) = log.split("NMI answered, it is here:\n").nth(1) else {
        return Err(format!("the probe reported no rip for the silent CPU\n{log}"));
    };
    let rip_line = rest.lines().next().unwrap_or("");
    if !rip_line.contains("deaf_window") {
        return Err(format!(
            "the rip resolved to `{}`, not to the spin the CPU was executing — a probe that \
             names the wrong instruction is worse than one that names none\n{log}",
            rip_line.trim(),
        ));
    }
    // And it comes back: an NMI interrupts, it does not kill. The witness has
    // to be the victim's own line, printed after it re-enables interrupts.
    // `Boot: complete` was the first attempt and is no witness at all — it is
    // printed at 225 ms, ten seconds before this window opens, and by cpu0 into
    // the boot log this drain does not even contain.
    if !log.contains("rejoined after") {
        return Err(format!(
            "the deafened CPU never said it was back — an NMI must interrupt a CPU, not kill \
             it\n{log}"
        ));
    }
    Ok(())
}

/// The blocked-task dump asked for where it may not be served, on one CPU:
/// `dump-in-blocking-pass` files one request in a kernel thread's blocking pass,
/// one in a user thread's, one in a pass entered above zero — which a thread
/// exiting from its syscall drives — and one during a report. Each staged pass
/// meets its request twice, with the clear a pass makes on entry between the
/// meetings, as a task woken behind it that blocks again would.
///
/// One CPU, so no sibling's pass serves what a pass left. The stages arm at the
/// SMP release, so one may fire under the boot's own load; one that has not,
/// `test_rs_dump_stage_load` fires: every task leaves the CPU before a quantum
/// ends and one is always ready, so no tick and no idle loop comes, and the only
/// pass entered at zero is the one the leaving CPU owes itself. The bound is the
/// construction's own and not a duration: `need_resched` is set by every pass
/// that leaves a request, and the Ring 3 exit check runs a pass entered at zero
/// while it is set — so zero returns to Ring 3 with the request pending.
///
/// Judged per request: it was left and that was said once, every report ran from
/// a pass entered at zero and none began inside another, the request filed during
/// a report got a report of its own, and the job finished clean.
pub fn dump_left_pending_is_owed(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            kernel_params: &["dump-in-blocking-pass"],
            smp: 1,
            ..Default::default()
        },
    );
    // Nothing is staged before this line, which the release prints at the first
    // pass after it: in the boot log, or after it on a boot that outran that pass.
    const ARMED: &str = "dump-in-blocking-pass: armed";
    let mut armed = qemu.boot_log().to_string();
    if !armed.contains(ARMED) {
        armed.push_str(&qemu.drain_until(Duration::from_secs(20), |line| line.contains(ARMED)));
    }
    if !armed.contains(ARMED) {
        return Err(format!("the actuator never armed — is `dump-in-blocking-pass` on?\n{armed}"));
    }
    let result = qemu.run_test("test_rs_dump_stage_load", Duration::from_secs(60));
    let log = format!("{armed}{}{}{}", result.before, result.serial, result.stdout);

    // A panic's message is the line after the one that says where.
    let mut panic = log.lines().skip_while(|line| !line.contains("PANIC")).take(2);
    if let Some(line) = panic.next() {
        return Err(format!(
            "a `PANIC` line is in the log: `{} {}`\n{log}",
            unstamped(line),
            unstamped(panic.next().unwrap_or(""))
        ));
    }
    // A request is filed only once the one before it is accounted for, so the
    // lines from one filing to the next are that request's.
    const FILED: &str = "files a request in ";
    let lines: Vec<&str> = log.lines().collect();
    let starts: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].contains(FILED)).collect();
    // What the filing line says, what the pass that left it says, and how many
    // reports follow: the user thread's blocking pass hosts the request filed
    // during a report.
    const STAGES: [(&str, &str, usize); 3] = [
        ("a blocking pass of a kernel thread", "met the request in a blocking pass", 1),
        ("a blocking pass of a user thread", "met the request in a blocking pass", 2),
        ("a pass entered at preempt depth", "met the request in a pass entered at preempt depth", 1),
    ];
    if starts.len() != STAGES.len() {
        return Err(format!("{} request(s) filed in a pass, not {}\n{log}", starts.len(), STAGES.len()));
    }
    for (filed, left, reports) in STAGES {
        let Some(at) = starts.iter().position(|&i| lines[i].contains(&format!("{FILED}{filed}"))) else {
            return Err(format!("no request was filed in {filed}\n{log}"));
        };
        let end = starts.get(at + 1).copied().unwrap_or(lines.len());
        let own = &lines[starts[at]..end];
        let count = |needle: &str| own.iter().filter(|line| line.contains(needle)).count();

        // Once per request: a line per meeting is two, and a line never re-armed
        // is none for the requests after the first.
        if count(left) != 1 || count("met the request in ") != 1 {
            return Err(format!(
                "the request filed in {filed} was left and that was said {} time(s), not once\n{log}",
                count("met the request in ")
            ));
        }
        let mut open = false;
        for line in own {
            if line.contains("=== blocked-task dump:") {
                if open {
                    return Err(format!("a report began inside another, after {filed}\n{log}"));
                }
                open = true;
            } else if line.contains("=== end of dump ===") {
                open = false;
            }
        }
        if count("=== end of dump ===") != reports || count("files a request during a report") != reports - 1 {
            return Err(format!(
                "{} complete report(s) and {} request(s) filed during one after {filed}, not {reports} and {}\n{log}",
                count("=== end of dump ==="),
                count("files a request during a report"),
                reports - 1,
            ));
        }
        const FROM: &str = " reports from ";
        if let Some(line) = own
            .iter()
            .find(|line| line.contains(FROM) && !line.contains("reports from a pass entered at preempt depth 0"))
        {
            return Err(format!("a pass that may not serve ran a report: `{}`\n{log}", unstamped(line)));
        }
        if count(FROM) != reports {
            return Err(format!("{} of {reports} report(s) said where they ran after {filed}\n{log}", count(FROM)));
        }
        const OWED: &str = " time(s) with its request pending";
        if count(OWED) != 1 {
            return Err(format!("the request filed in {filed} was accounted for {} time(s)\n{log}", count(OWED)));
        }
        if let Some(line) = own
            .iter()
            .find(|line| line.contains(OWED) && !line.contains("returned to Ring 3 0 time(s)"))
        {
            return Err(format!(
                "a cpu that left a request went back to Ring 3 without serving it: `{}`\n{log}",
                unstamped(line)
            ));
        }
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "the job whose passes were staged did not finish clean: exit {:?}\n{log}",
            result.exit_code
        ));
    }
    Ok(())
}

/// A kernel line without its `[kernel <seconds> cpuN] ` stamp, which differs on every boot: a
/// quoted line that kept it would make every red of a rerun a different one.
fn unstamped(line: &str) -> &str {
    let line = line.trim();
    line.strip_prefix("[kernel ").and_then(|rest| rest.split_once("] ")).map_or(line, |(_, said)| said)
}
