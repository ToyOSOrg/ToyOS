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
    refused_claim(&log, bench.claims, "its capability list ends at a link the PCI spec forbids")?;
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
/// boot config's own spelling.
///
/// The three arms that refuse a claim read this one judge, so a kernel that
/// answered a refusal by logging it and handing the function over anyway is red
/// wherever the refusal is reached. `slot_space` put back below `place_bars`
/// reds on the two unspent lines.
pub fn refused_claim(log: &Serial, claims: &str, why: &str) -> Result<(), String> {
    let refused = functions_named(log, "NOT HANDED OVER")?;
    if refused.is_empty() || refused.iter().any(|at| *at != CLAIMED_AT) {
        return Err(format!(
            "the claim this judges is the one on {CLAIMED_AT}; this console refused \
             {refused:?}:\n{}",
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

/// A machine with no NVMe controller must boot.
///
/// `.expect("NVMe: no controller found")` killed it at 0.08 s — before
/// storage, before a console on the target laptop, and with the screen still
/// showing whatever the last checkpoint painted. It is the same class M1
/// closed for xHCI, on a different controller, and the same class the
/// designation stamp closed one layer up: absence of storage is a
/// configuration, not a failure.
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
    log.must_say("NVMe: no controller on this machine")?;
    log.must_say("Boot: complete")?;
    Ok(())
}

/// How long the guest spins. The storm arms about 190 ms after the spinner
/// starts — a million syscalls at its measured rate — and this is what covers a
/// slow arming plus the storm itself on a shard with company.
const SPIN_SECS: u32 = 10;

/// The kernel's own summary line, printed last so a drain that ends on it has
/// every per-CPU line and the symbolized `rip` under it already.
const NMI_REPORT: &str = "syscall-window-nmi: sent=";

/// The storm's own word, before it sends, that it holds the victim inside the
/// entry: which CPU, the `rsp` it is held at, and where the entry spins.
const HELD: &str = "syscall-window-nmi: held cpu=";

/// The storm's word that it found nobody to hold, before it sprays.
const HELD_NOBODY: &str = "syscall-window-nmi: held nobody";

/// The storm's word that the hold is over: an arrival under this line is a
/// sprayed one.
const RELEASED: &str = "syscall-window-nmi: released cpu=";

/// A held CPU's word that its entry spent the ask's whole budget and left the
/// window with nobody having released it.
const HOLD_EXPIRED: &str = "syscall-window-nmi: hold expired cpu=";

/// The storm's word that the victim it released made no syscall before the
/// spray went out.
const NO_SYSCALL_AFTER_RELEASE: &str = "of its release, so the spray's first samples may be";

/// What the storm said of the hold it arranged.
struct Hold {
    cpu: u64,
    /// The user `rsp` the victim was held at.
    rsp: u64,
    /// Where the entry spins while held.
    spin: std::ops::Range<u64>,
}

/// The premise both arms rest on, read off the capture: the CPU the storm held
/// inside the entry's window and the `rsp` it held it at, which has to be the
/// user's or the hold was not the window.
fn held_in_window(capture: &str) -> Result<Hold, String> {
    let Some(line) = capture.lines().find(|l| l.contains(HELD)) else {
        return Err(if capture.contains(HELD_NOBODY) {
            format!(
                "the storm says it held nobody, so the premise every verdict rests on was not \
                 arranged and this run says nothing about vector 2's IST\n{capture}"
            )
        } else {
            format!("the capture has no `{HELD}` line and no `{HELD_NOBODY}` line\n{capture}")
        });
    };
    let cpu = field(line, "cpu=")?;
    let part = |name: &str| hex(line, name).ok_or_else(|| format!("no {name}0x… field in {line:?}"));
    let (rsp, spin) = (part("rsp=")?, part("spin=")?..part("end=")?);
    if rsp >= toyos_userbound::USER_TOP {
        return Err(format!(
            "cpu{cpu} was held at rsp={rsp:#x}, which is not a user address: a hold on a kernel \
             stack is not the window, and an NMI there finds a stack the CPU may push \
             on\n{capture}"
        ));
    }
    Ok(Hold { cpu, rsp, spin })
}

/// Refuses a capture in which the kernel says a hold ended by the entry's own
/// bound: nobody released that CPU, so nothing under the line is the run the
/// storm arranges.
fn hold_expired(capture: &str) -> Result<(), String> {
    match capture.lines().find(|l| l.contains(HOLD_EXPIRED)) {
        Some(line) => Err(format!(
            "the kernel says a hold ended by the entry's own bound and not by the storm's \
             release: `{}`\n{capture}",
            line.trim(),
        )),
        None => Ok(()),
    }
}

/// A held CPU with neither end of its hold in the capture: what is absent, and
/// no reason for it.
fn never_released(hold: &Hold, capture: &str) -> String {
    format!(
        "cpu{} was held inside the entry and the capture has no `{RELEASED}` line and no \
         `{HOLD_EXPIRED}` line\n{capture}",
        hold.cpu,
    )
}

/// The `name`N field of a key=value report line, by name and not position.
fn field(line: &str, name: &str) -> Result<u64, String> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(name)?.parse::<u64>().ok())
        .ok_or_else(|| format!("no {name}N field in {line:?}"))
}

/// The `field`0x… value on `line`, up to sixteen hex digits.
fn hex(line: &str, field: &str) -> Option<u64> {
    let rest = line.split(field).nth(1)?;
    let digits: String = rest.trim_start_matches("0x").chars().take(16).collect();
    u64::from_str_radix(&digits, 16).ok()
}

/// An NMI delivered where CPL is 0 and `rsp` is still the user's, and a machine
/// that carries on.
///
/// **One boot, and the two negative controls are `syscall_window_nmi_controls`'s
/// two.** All three used to be one name and it priced at 19,740 ms on the hosted
/// lane against a 10,000 ms ceiling — three Metal boots of 3,000 NMIs each. What
/// belongs per pull request is the property: the window is reachable, arrivals
/// land in it, and the machine survives them. What the controls establish is that
/// the property is not vacuous, which is a claim about the *instrument* and moves
/// to the nightly tier.
///
/// **The window.** `SYSCALL` switches no stack, so `arch::syscall`'s entry runs
/// three instructions at CPL 0 with the user's `rsp` and its exit one more
/// between `pop rsp` and `sysretq`. A frame the CPU builds there is a supervisor
/// write to a user page: SMAP refuses it, the `#PF` lands on the same stack, and
/// the machine takes a `#DF`. `arch::idt`'s IST2 row is the fix and this is what
/// says the row is load-bearing.
///
/// **The first arrival is arranged; where the sprayed ones land is the
/// accelerator's answer, and on KVM it is the host's.** Before it sprays, the
/// storm holds the victim inside the entry — `nmi_gate::hold`'s word, which the
/// entry acknowledges and spins on at CPL 0 on the user's stack — and aims one
/// NMI at it there. That arrival is every host's, and is asserted on every
/// host: the `held cpu=… rsp=…` line the storm prints before the send, with an
/// `rsp` in the user half, and a `held` count in its report that the held CPU
/// itself keeps — a window arrival taken while its own word still had both
/// bits — whose frame stands at the held `rsp` with a `rip` inside the entry's
/// spin. The spray's landings are not.
///
/// Under TCG, QEMU checks for a pending interrupt between translation
/// blocks and `syscall` ends one, so a pending NMI is delivered at
/// `syscall_entry+0`: the dev host reads 36 to 58 arrivals per 3,000, run after
/// run. Under KVM an NMI to a running vCPU is a host kick, a VM exit and an
/// injection at the next VM entry — and **which instruction that entry is
/// depends on where the kick's exit landed**, which is a property of the host
/// and not of the guest. Both extremes are measured on the hosted lane, on the
/// spray alone:
///
/// - **0 of 6,000** (run 32584121311, two boots, with 2,451 and 438 of the same
///   NMIs arriving in Ring 3, so the aim was right and the injection point was
///   simply somewhere else);
/// - **64 of 64** (run 32587665835 `guest (9)`, `window=64 ring3=0 spun=16`,
///   the exit landing on the `syscall` boundary and the injection on the entry's
///   first instruction every time, so the storm ended at `ENOUGH` after 64
///   deliveries).
///
/// So a sprayed in-window count asserted on KVM would be asserting about the
/// host, in either direction: a floor reds the first host and a ceiling reds
/// the second. CI's guest lane is KVM only (`tests/CLAUDE.md`), and the
/// accelerator is read off the argv this boot was built from — the same
/// `-accel kvm` decision `qemu_command` made, not a re-derivation of it:
///
/// - **under TCG** the derived count is asserted as [`SAME_ORDER`] below, with
///   the held arrival taken out of it first — on a run whose victim was running
///   when sampled (victim-located arrivals at or under its own traversals); a
///   parked victim is the declared degradation at that check, printed and not
///   judged;
/// - **under KVM** the sprayed counts are printed as the instrument's verdict,
///   and what is asserted is what every host witnesses: the held arrival, nine
///   of ten aimed NMIs delivered, at least one of them arriving somewhere only
///   the victim can be — in Ring 3 *or* in the window — no window arrival with
///   a `rip` outside the entry, and no `#DF`.
///
/// The window itself is gated on both by `syscall_window_nmi_controls`, whose
/// `nmi-without-ist` arm double faults at `syscall_entry` on the held arrival,
/// with `cr2 = rsp - 8` at the `rsp` it was held at. `wake_storm_cost` is the
/// shape this follows: whether an instrument can read the thing is the
/// instrument's verdict, printed, and the derived assertion is made only on a
/// run that can read it.
///
/// **The derivation, where it applies.** Every iteration of the spinner's loop
/// passes through the window exactly once and through Ring 3 exactly once, so
/// the two counts differ only by how many points an NMI can be delivered at
/// inside each: a few instructions either side under a delivery model uniform
/// over instructions, and under TCG one block boundary in the window against
/// two or three on the user side. Both readings say one traversal each within a
/// small factor, and [`SAME_ORDER`] is the bound: an order of magnitude, which
/// no reading of the delivery model reaches and a classification that has
/// stopped tracking the loop fails at once. The held arrival is not a sample of
/// the spray and is subtracted before the ratio.
///
/// Measured, dev host, TCG, `-smp 4`, 3,000 NMIs sent: **47 window arrivals
/// against 136 in Ring 3** aimed, **36 against 122** while the storm still
/// sprayed every sibling.
///
/// The bound is not the teeth on its own. What says the count means the window
/// is every counted arrival's own `rip`: the kernel holds each against
/// `syscall_entry`'s extent and the report's `outside` has to be zero, on both
/// accelerators. Under TCG the first *sprayed* one is also symbolized by the
/// kernel and asserted against `syscall_entry`: `dump_nmi_probe`'s rule, that a
/// probe naming the wrong instruction is worse than one naming none, read off
/// the symbol table rather than off the labels `outside` is judged by. The held
/// arrival cannot say either — it is inside the entry by arrangement.
pub fn syscall_window_nmi(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    // One traversal each per iteration, so the two counts are of one order.
    const SAME_ORDER: u64 = 10;

    let survived = storm(test_config, c_bins, rust_bins, &["syscall-window-nmi"], SPIN_SECS, |l| {
        l.contains(NMI_REPORT) || l.contains(HOLD_EXPIRED)
    })?;
    if survived.contains("DOUBLE FAULT") {
        return Err(format!(
            "an NMI in the syscall window still took the machine down — vector 2's IST index \
             is not doing what the table says\n{survived}"
        ));
    }
    hold_expired(&survived)?;
    let Some(report) = survived.lines().find(|l| l.contains(NMI_REPORT)) else {
        return Err(match held_in_window(&survived) {
            Ok(hold) if !survived.contains(RELEASED) => never_released(&hold, &survived),
            _ => format!("the storm never reported — is `syscall-window-nmi` on?\n{survived}"),
        });
    };
    // The premise before any verdict: the storm's own word that it held the
    // victim inside the entry at the user's `rsp`, and the held CPU's own count
    // of the arrival it took there.
    let hold = held_in_window(&survived)?;
    if let Some(line) = survived.lines().find(|l| l.contains(NO_SYSCALL_AFTER_RELEASE)) {
        return Err(format!(
            "the storm sprayed without waiting out the hold's end, so a window arrival of this \
             run may be the released victim still inside the entry, which vouches for any \
             classifier: `{}`\n{survived}",
            line.trim(),
        ));
    }
    let (sent, seen) = (field(report, "sent=")?, field(report, "seen=")?);
    let (window, ring3) = (field(report, "window=")?, field(report, "ring3=")?);
    let (spun, held) = (field(report, "spun=")?, field(report, "held=")?);
    let outside = field(report, "outside=")?;
    let Some(released) = survived.lines().find(|l| l.contains(RELEASED)) else {
        return Err(never_released(&hold, &survived));
    };
    // Printed and not judged: how much of the entry's budget a hold takes is
    // the host's pace.
    let (turns, budget) = (field(released, "turns=")?, field(released, "budget=")?);
    eprintln!(
        "  [nmi-window] cpu{} held at rsp={:#x} for {turns} of the entry's {budget} turns; {sent} \
         sent, {seen} taken, {window} in the window with {held} of them the held arrival and \
         {outside} outside the entry, {ring3} in Ring 3, {spun} syscalls made under the storm",
        hold.cpu, hold.rsp,
    );
    // Every arrival and every accelerator: the kernel holds each `window`
    // frame's `rip` against the entry's own extent, which is the only code that
    // runs at CPL 0 on a user's `rsp`.
    if outside != 0 {
        return Err(format!(
            "{outside} of {window} window arrivals had a `rip` outside `syscall_entry`: either the \
             classifier counts Ring 0 frames that are not the window, or there is a second place \
             this kernel runs at CPL 0 on a user's `rsp`\n{survived}"
        ));
    }
    if held == 0 {
        return Err(format!(
            "cpu{} was held inside the entry at rsp={:#x} and took no window arrival while its \
             hold word had both bits — either the NMI aimed at it was not delivered inside the \
             hold, or a Ring 0 frame with a user `rsp` is classified as something else; either \
             way the premise is missing and nothing below is a verdict on the window\n{survived}",
            hold.cpu, hold.rsp,
        ));
    }
    if window < held {
        return Err(format!(
            "the report counts the held arrival in the window and {window} window arrivals in \
             all — one counter did not read the other's decision\n{survived}"
        ));
    }
    // The held arrival's own frame, which the kernel keeps apart from the
    // sprayed ones': at the `rsp` the storm read before it sent, and inside the
    // spin the entry was held in.
    let Some(arrival) = survived.lines().find(|l| l.contains("the held arrival had rip=")) else {
        return Err(format!("the report counts a held arrival and names no frame for it\n{survived}"));
    };
    let (rip, rsp) = (hex(arrival, "rip="), hex(arrival, "rsp="));
    if rsp != Some(hold.rsp) || !rip.is_some_and(|rip| hold.spin.contains(&rip)) {
        return Err(format!(
            "the held arrival's frame is rip={rip:#x?} rsp={rsp:#x?}, and cpu{} was held at \
             rsp={:#x} spinning in {:#x?}: the arrival counted as the held one is not the one \
             the hold arranged\n{survived}",
            hold.cpu, hold.rsp, hold.spin,
        ));
    }

    // **A low delivery ratio is the host, not the kernel — unless the victim
    // also made no progress.** A victim that an NMI *ended* takes the machine
    // down with it (the `_controls` arm shows an IST-less NMI double-faults and
    // halts), so a real death never reaches this report at all — the guest dies
    // and the harness times it out. What lowers `seen` here instead is a loaded
    // host delivering NMIs slower than the sender's own deadline: an APIC latches
    // at most one pending NMI, so under load `sent` outruns `seen` with the
    // machine perfectly alive (1,854 of 3,000 on a dev host running four other
    // suites, run 32637… local, `spun=69071`). So this fires only when few were
    // taken *and* the victim completed no syscall under the storm — the stall
    // signature, which the aim check below also catches.
    if seen * 10 < sent * 9 && spun == 0 {
        return Err(format!(
            "{sent} NMIs were sent, only {seen} taken, and the victim completed no syscall under \
             the storm — that pair is a CPU that stopped running, which is what an NMI that ends \
             one looks like from here\n{survived}"
        ));
    }
    // **The aim is proved by any of three witnesses, and requiring only the
    // first two reds a run where the victim was mid-syscall the whole storm.** A
    // Ring 3 frame and a Ring 0 frame with a user `rsp` (the window) are states
    // only the running spinner can be in — but so is a Ring 0 frame *inside the
    // syscall it is spamming*, which the report counts as neither `window` nor
    // `ring3`, and an idle sibling is in a Ring 0 frame too, so that count alone
    // cannot tell them apart. `spun` disambiguates it: the victim's own syscalls
    // completed under the storm, which only the running spinner produces (an
    // idle CPU makes none). So the aim missed only when all three are zero. This
    // was `window=0 ring3=0 spun=34 ring0=3000` on a hosted lane (run
    // 32637767026, `guest (8)`): every NMI caught the spinner inside `SYS_GETPID`
    // and the old `window + ring3 == 0` called a perfect aim a miss.
    if window + ring3 == 0 && spun == 0 {
        return Err(format!(
            "not one of {seen} NMIs arrived with a Ring 3 frame or in the window, and the aimed \
             CPU completed no syscall under the storm — all three are states only the CPU \
             running the spinner produces, so the storm was aimed at a CPU that was not running \
             it and this run measured an idle loop\n{survived}"
        ));
    }

    if kvm_accelerated() {
        // **The instrument's verdict, not the kernel's**, and on KVM the
        // instrument's answer is the host's: the injection lands where the
        // kick's VM exit did, so one hosted host put none of 3,000 in the window
        // and another put 64 of 64 there (this function's header carries both).
        // Neither number says anything about the kernel, so neither is asserted.
        //
        // `spun` is printed and not asserted here for the same reason: a storm
        // that stops at its 64th window arrival is over in a few dozen
        // deliveries, so the count measures how fast `ENOUGH` arrived — 16
        // syscalls under a 64-NMI storm is the instrument working, not a stall.
        // What the victim's liveness rests on is the arrival counts above and
        // the delivery ratio, which are the same on every host.
        eprintln!(
            "  [nmi-window] KVM delivered {} of {seen} sprayed NMIs into the window and {ring3} \
             in Ring 3, with {spun} syscalls made under the storm: where this accelerator \
             injects is the host's business, so what this run gates is the held arrival and \
             that the machine took {sent} aimed NMIs with IST2 in place and went on working",
            window - held,
        );
        return Ok(());
    }

    // **Under TCG the arrivals themselves imply the syscalls**, whichever limit
    // ended the storm: a window arrival is an NMI taken *inside* a syscall
    // entry, and that syscall then returns from the handler and completes, which
    // is what increments this counter. The dev host reads tens of them per
    // thousand deliveries, so a zero here is a CPU that stopped running Ring 3
    // code rather than a storm that ended early. It is not asserted on KVM for
    // the reason above: there a storm can be over in 64 deliveries, and the
    // count then measures how fast the ceiling arrived.
    if spun == 0 {
        return Err(format!(
            "the victim made no syscall at all while {seen} NMIs were delivered to it — it \
             stopped running Ring 3 code under the storm\n{survived}"
        ));
    }

    // **A starved victim voids the sampling the window verdicts rest on, and
    // that is the declared degradation rather than a red.** The derivation
    // samples a *running* spinner's loop, and a running victim collects fewer
    // victim-located arrivals than traversals of its own (64+155 against 794
    // alone on this host); a victim the host mostly keeps parked collects
    // them piled at one point — the recorded red is `window=0 ring3=77` on 18
    // traversals, twelve wide beside a second suite. What such a run still
    // gated is everything above: survival, delivery, and an aim only the
    // victim can witness.
    if window + ring3 > spun {
        eprintln!(
            "  [nmi-window] declared degradation: {} victim-located arrivals against {spun} \
             traversals of the victim's own loop — the arrivals piled onto a parked CPU, so \
             the window-placement verdicts are not rendered on this run; survival, delivery \
             and aim were",
            window + ring3,
        );
        return Ok(());
    }

    // The spray alone: the held arrival was aimed, not sampled.
    let sprayed = window - held;
    if sprayed == 0 {
        return Err(format!(
            "{sent} NMIs were sent and {seen} taken under TCG, and not one sprayed NMI landed \
             in the syscall window — this accelerator delivers at translation-block boundaries \
             and `syscall` ends one, so the spray proved nothing about the stack the CPU pushes \
             on\n{survived}"
        ));
    }
    if sprayed * SAME_ORDER < ring3 {
        return Err(format!(
            "{sprayed} sprayed window arrivals against {ring3} in Ring 3. Every iteration passes \
             through both exactly once, so they are of one order; a {SAME_ORDER}x shortfall \
             says the arrivals are not being classified where they land\n{survived}"
        ));
    }
    // What makes the count a claim about the window rather than about some
    // other Ring 0 frame with a low `rsp`: the kernel symbolizes the first
    // sprayed one it saw, and it has to be the entry.
    let Some(rest) = survived.split("the first sprayed window arrival was here:\n").nth(1) else {
        return Err(format!(
            "the report named no rip for the first sprayed window arrival\n{survived}"
        ));
    };
    let named = rest.lines().next().unwrap_or("");
    if !named.contains("syscall_entry") {
        return Err(format!(
            "the first sprayed window arrival resolved to `{}`, not to the syscall entry — a \
             Ring 0 frame with a user `rsp` somewhere else is a different finding, and this test \
             is not measuring it\n{survived}",
            named.trim(),
        ));
    }
    Ok(())
}

/// Whether this host's guests run under KVM, read off the argv a boot is built
/// from.
///
/// **The decision itself rather than a second reading of it**: `qemu_command`
/// puts `-accel kvm` there when `toyos_build::kvm_usable()` says so, and
/// `profile_argv` is that same builder. A CPUID probe in the guest would be a
/// second place that can be told the wrong answer, and `virtio_net_no_msix` and
/// `diskless_boot` already assert about a boot by reading its argv.
fn kvm_accelerated() -> bool {
    qemu::profile_argv(&storm_options(&[]))
        .windows(2)
        .any(|w| w[0] == "-accel" && w[1] == "kvm")
}

/// The two negative controls on [`syscall_window_nmi`], which is where the
/// property is asserted and this is where it is shown not to be vacuous.
///
/// **Nightly.** Two Metal boots, and both end in a halted machine that has to be
/// drained past its own report — which is what the price is. A control is a
/// claim about the
/// instrument rather than about the kernel under review: it says the same test,
/// run against a kernel with the defect, reds. That does not change per pull
/// request, and the fixed arm reds per pull request if the kernel does.
///
/// `#MC` has no control here and cannot have one: CR4.MCE is set and nothing in
/// QEMU raises a machine check. Its IST index rides the same table column NMI's
/// does, plus `arch::idt`'s compile-time assertion over that table.
pub fn syscall_window_nmi_controls(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    // The first control: the same boot with vector 2's IST index taken off.
    // Everything else — the handler, the gate, the storm, the hold, the spinner
    // — is the same, so what the `#DF` below measures is the one byte.
    // Drained past the header, not to it: `double_fault_handler` prints the
    // address that started the chain, then the registers, then the backtrace
    // that carries the symbol every assertion below reads, and only then this.
    let unfixed = storm(
        test_config,
        c_bins,
        rust_bins,
        &["syscall-window-nmi", "nmi-without-ist"],
        SPIN_SECS,
        |l| l.contains("Scanning kernel stack") || l.contains(HOLD_EXPIRED),
    )?;
    hold_expired(&unfixed)?;
    // The premise before the consequence: a control whose stimulus missed can
    // only say what an absent defect says, and this one says which it was.
    let hold = held_in_window(&unfixed)?;
    let line_of = |what: &str| unfixed.lines().position(|l| l.contains(what));
    let Some(df_at) = line_of("DOUBLE FAULT") else {
        if line_of(RELEASED).is_none() {
            return Err(never_released(&hold, &unfixed));
        }
        return Err(format!(
            "cpu{} was held inside the entry at rsp={:#x} with no IST on vector 2 and an NMI \
             aimed at it, and the machine survived — the CPU took that NMI at CPL 0 on a user \
             page without the stack IST2 provides and nothing refused the frame, so on this \
             machine the row is not what stands between the window and a #DF\n{unfixed}",
            hold.cpu, hold.rsp,
        ));
    };
    // The held arrival's `#DF` and not a sprayed one's: the storm says when the
    // hold ended, and the death has to be above that line or the victim was
    // already out of the hold when it died.
    if line_of(RELEASED).is_some_and(|released| released < df_at) {
        return Err(format!(
            "the storm released cpu{} before the #DF: the NMI aimed at the hold did not take the \
             machine down, and the one that did was sprayed at a CPU nobody held\n{unfixed}",
            hold.cpu,
        ));
    }
    let df = unfixed.lines().nth(df_at).unwrap_or_default();
    eprintln!("  [nmi-window] without IST2: {}", df.trim());
    let df_cpu = df
        .split("on CPU ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok());
    if df_cpu != Some(hold.cpu) {
        return Err(format!(
            "the double fault was on CPU {df_cpu:?}, not on cpu{} the storm held — a #DF on any \
             other CPU is a different death\n{unfixed}",
            hold.cpu,
        ));
    }
    if !unfixed.contains("syscall_entry") {
        return Err(format!(
            "the control double faulted somewhere other than the syscall entry\n{unfixed}"
        ));
    }
    // **The exact signature, and the reason this is a control and not a
    // coincidence**: the `#DF` stands inside the spin the victim was held in,
    // at the `rsp` it was held at, and the address the CPU faulted on is the
    // first qword of the frame it was trying to push there, one below it. A #DF
    // for any other reason does not put `cr2` there, one on any other stack is
    // not the held one, and a sprayed arrival's is at the entry's first
    // instruction and not in the spin.
    let report: Vec<&str> = unfixed.lines().skip(df_at).collect();
    let cr2 = report.iter().find_map(|l| hex(l, "cr2="));
    let rip = report.iter().find_map(|l| hex(l, "rip="));
    let rsp = report.iter().find_map(|l| hex(l, "rsp="));
    match (cr2, rip, rsp) {
        (Some(cr2), Some(rip), Some(rsp))
            if rsp == hold.rsp && cr2 == rsp.wrapping_sub(8) && hold.spin.contains(&rip) =>
        {
            eprintln!(
                "  [nmi-window] without IST2: the #DF stands at the held rsp={rsp:#x} with \
                 rip={rip:#x} inside the spin, and cr2={cr2:#x} is rsp-8, the frame's first qword"
            );
        }
        (cr2, rip, rsp) => {
            return Err(format!(
                "the control's #DF reports cr2={cr2:#x?} rip={rip:#x?} rsp={rsp:#x?}, with the \
                 victim held at rsp={:#x} spinning in {:#x?}; the fault this stages is the \
                 frame's own first qword at the held rsp-8 from inside that spin, so this is a \
                 different death\n{unfixed}",
                hold.rsp, hold.spin,
            ));
        }
    }

    // The second control: an NMI handler that returns early through `iretq`
    // un-masks NMIs while still standing on IST2, which is the one way a second
    // NMI can enter on that stack. The check has to fire and say so.
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
/// One declaration of the machine every arm boots, so that the argv
/// [`kvm_accelerated`] reads is the argv the boot is built from.
fn storm_options(params: &'static [&'static str]) -> BootOptions {
    BootOptions {
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
    }
}

fn storm(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    params: &'static [&'static str],
    secs: u32,
    done: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let mut qemu =
        QemuInstance::boot_with_options(test_config, c_bins, rust_bins, storm_options(params));
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
pub fn dump_nmi_probe(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            kernel_params: &["dump-deaf-cpu"],
            ..Default::default()
        },
    );
    // 3 s is the actuator's earliest arming, not its schedule: cpu0 only looks
    // once per idle-loop iteration, and on a settled guest the next thing that
    // wakes it is the 10 s health tick. Add 400 ms of deafness and the dump's
    // 250 ms kick budget, and 20 s is the first round number that clears it.
    //
    // **A ceiling now rather than the run.** The guest neither exits nor halts
    // here — an NMI interrupts a CPU, it does not kill it — so a plain drain
    // paid the whole twenty seconds on every green run, against a guest that
    // was done in about a third of it. Both markers, and neither implies the
    // other's order: the dump is requested while the victim is deaf and the
    // victim announces its own return when the 400 ms window closes, so which
    // of the two lands last is a fact about how long the report takes rather
    // than about the machine.
    let dumped = std::cell::Cell::new(false);
    let rejoined = std::cell::Cell::new(false);
    let log = qemu.drain_until(Duration::from_secs(20), |line| {
        dumped.set(dumped.get() || line.contains("=== end of dump ==="));
        rejoined.set(rejoined.get() || line.contains("rejoined after"));
        dumped.get() && rejoined.get()
    });

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
