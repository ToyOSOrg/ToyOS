//! Identifiers a tree may not name, and the exceptions that are named instead.
//!
//! **Clippy runs now, and these scans are what it cannot say.** The `host` job
//! in `.github/workflows/host-tests.yml` runs default clippy with warnings
//! denied over three trees on every pull request — the host workspace
//! (`--workspace --all-targets`), the kernel (`--target x86_64-unknown-none`)
//! and the bootloader (`--target x86_64-unknown-uefi`) — so a `clippy.toml` is
//! no longer a wall with nothing behind it.
//!
//! What is behind it is still not these six scans. `disallowed-methods` could
//! take the first one, and would lose what makes it useful: the exceptions
//! below are per file *and per line count*, so an added `mem::forget` beside a
//! permitted one reds, which a name-based allow list cannot express. The second
//! and third scans ask whether an identifier is absent from the tree, which is
//! not a question any lint asks at all. So the first scan bans, over
//! `kernel/src` and `toyos-sched/src`, the methods that take an object's
//! lifetime out of its `Arc`'s hands — `Arc::into_raw`, `Arc::from_raw`, the
//! two strong-count adjusters — and `mem::forget`. It runs in
//! `cargo test --lib`, on every machine that builds this tree, in milliseconds.
//!
//! The exceptions are per file and per line count, so an *added* `forget`
//! beside a permitted one is a red rather than a silence.
//!
//! The second scan enforces that there is no global registry: a name a process
//! could present and have resolved for it is the thing this architecture
//! deleted, so the registry's identifiers must be gone from the code rather
//! than merely unused. It reads
//! **code only** — comments and string literals are stripped first — because
//! the history of what a name used to mean is worth keeping and the
//! retired-syscall gravestone table names every one of them as a string on
//! purpose.
//!
//! The third runs the same walk over [`RETIRED_ABI_NAMES`]: every syscall,
//! `SYS_DEBUG` action and inbox op code this project has deleted. Its
//! *number* is what is retired, and a number is not a thing a scan can look
//! for — the name that used to carry it is.
//!
//! The fourth names two files, the pipe ring and the user-copy windows, where a
//! slice or an exclusive reference would claim what the mapping does not give.
//!
//! The fifth and sixth are the dependency bar: every argument the host's code
//! hands to `Command::new`, and every committed file whose bytes are not text
//! with the digest `NOTICE` records for it. Both declare rather than judge —
//! the bar is `CLAUDE.md`'s and its standing failures are the owner's — and
//! both refuse the undeclared rather than the known-bad, which is what a scan
//! for a spelling cannot do.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// One banned identifier: what it is, why, and where it is nonetheless allowed.
struct Ban {
    needle: &'static str,
    why: &'static str,
    /// `(path relative to the repository root, how many times)`.
    allowed: &'static [(&'static str, usize)],
}

/// Trees the object layer's lifetime rules govern.
const TREES: &[&str] = &["kernel/src", "toyos-sched/src"];

const BANS: &[Ban] = &[
    Ban {
        needle: "Arc::into_raw",
        why: "an object's lifetime is its Arc's; a raw pointer out of one is a \
              refcount nobody owns",
        allowed: &[],
    },
    Ban {
        needle: "Arc::from_raw",
        why: "the other half of the same hole",
        allowed: &[],
    },
    Ban {
        needle: "Arc::increment_strong_count",
        why: "hand-rolled refcounting is the bug class the object layer deletes",
        allowed: &[],
    },
    Ban {
        needle: "Arc::decrement_strong_count",
        why: "as above, and this one is the half that frees",
        allowed: &[],
    },
    Ban {
        // Not a refcount hazard by itself — the ban is about intent. A `forget`
        // is a statement that something is never given back, and in a kernel
        // that does not unwind it reads exactly like a `Drop` somebody meant to
        // rely on.
        needle: "mem::forget",
        why: "a resource with no giver-back is a leak unless the reason is at \
              the call site",
        allowed: &[
            // The GPU is never torn down, so the cursor pages outlive every
            // process that could name them.
            ("kernel/src/drivers/gop.rs", 1),
            // dlmalloc owns the page from here on.
            ("kernel/src/mm/alloc.rs", 1),
            // `DmaPool::leak`, and here the leak *is* the statement: a device
            // this kernel has bound is bound for the boot, and the
            // `Dma<'static>` it answers with is the only way to say so in the
            // type. It replaced four `static Lock<Option<DmaPool>>`s that
            // leaked the same pages by never being cleared, where nothing said
            // so at all.
            ("kernel/src/mm/dma.rs", 1),
            // Both in `cpu.rs`'s test module, and both are the drop bomb
            // rather than a leak: `Task`'s "the only legal death is
            // `DeadTask::finalize`" is a scheduler invariant, so a test that
            // deliberately ends with a live task — which is what most of these
            // arms do — may not drop its world, and a registration held
            // past a park it staged by hand is the same statement.
            ("toyos-sched/src/cpu.rs", 2),
        ],
    },
    // `toyos_untrusted::Untrusted` has no accessor, no cast, no arithmetic and
    // no `From`, so a number that crossed a trust boundary cannot reach an
    // index or a length except through `index(len)` or `at_most(bound)`. That
    // closes every *accidental* way out, which is every one of the filed sites:
    // each was a cast or a bare comparison somebody wrote without noticing they
    // were deciding anything.
    //
    // What typing alone cannot close is a bound that is not a bound. A caller
    // may write `at_most(u64::MAX)` and get the value straight back, and no
    // signature distinguishes that from a real ceiling. So the deliberate form
    // is banned here instead — which is the difference between "nothing stops
    // the ninth site" and "one scan does". These five needles are the whole of
    // it, because a bound written as a type's own maximum is the only shape
    // that admits every value.
    Ban {
        needle: "at_most(u64::MAX",
        why: "a bound of u64::MAX admits every value: it is an unwrap wearing a \
              check's name. If there is genuinely no ceiling then the value is \
              not a length, and it does not want this exit",
        allowed: &[],
    },
    Ban {
        needle: "at_most(u32::MAX",
        why: "as above, for a value already narrower than the bound it names",
        allowed: &[],
    },
    Ban {
        needle: "at_most(u16::MAX",
        why: "as above",
        allowed: &[],
    },
    Ban {
        needle: "at_most(u8::MAX",
        why: "as above",
        allowed: &[],
    },
    Ban {
        needle: "index(usize::MAX",
        why: "the same hole in the other exit: every value indexes a table of \
              usize::MAX entries, and no such table exists",
        allowed: &[],
    },
    // A 4096 typed into a kernel constant is the page size almost every time,
    // and a private copy of it is a value that does not move when the export
    // does. `mm::PAGE_SIZE` is the export; the exceptions below are the
    // 4096-byte things that are not a page — a probe's line count, a guard
    // page's *size in bytes* where the page size is not what decides it, a ring
    // capacity in entries, a path length, a device's own buffer or register
    // window — each one a row somebody wrote on purpose.
    Ban {
        needle: ": usize = 4096",
        why: "a private page size is a copy of `mm::PAGE_SIZE` that stops moving \
              with it",
        allowed: &[
            // Cache lines to walk, not bytes.
            ("kernel/src/arch/control_regs.rs", 1),
            // Two guard-page sizes: the mapping is 4 KiB because a guard is one
            // hardware page, and `PAGE_SIZE` is 2 MiB territory here.
            ("kernel/src/arch/percpu.rs", 2),
            // A device's TX buffer.
            ("kernel/src/drivers/virtio_console.rs", 1),
            // A VT-d table is 4 KiB by the specification, not by this kernel.
            ("kernel/src/iommu/vtd/table.rs", 1),
            // Ring entries.
            ("kernel/src/trace.rs", 1),
            // A path length, in bytes.
            ("kernel/src/vfs.rs", 1),
        ],
    },
    Ban {
        needle: ": u64 = 4096",
        why: "as above, in the other width",
        allowed: &[
            // A VT-d register window, by the specification.
            ("kernel/src/iommu/vtd/mod.rs", 1),
            // The export itself, and the one place the literal lives.
            ("kernel/src/mm/mod.rs", 1),
        ],
    },
    Ban {
        needle: ": u32 = 4096",
        why: "as above, in the other width",
        allowed: &[
            // A device's RX buffer.
            ("kernel/src/drivers/virtio_net.rs", 1),
            // The block size this driver reads a disk in.
            ("kernel/src/drivers/xhci/wait/msc.rs", 1),
        ],
    },
];

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `(file, count)` where `needle` appears, over [`TREES`].
fn occurrences(needle: &str) -> Vec<(String, usize)> {
    let root = repo_root();
    let mut files = Vec::new();
    for tree in TREES {
        rust_files(&root.join(tree), &mut files);
    }
    let mut found = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let n = text.matches(needle).count();
        if n > 0 {
            found.push((rel(&root, &path), n));
        }
    }
    found
}

/// The names the global registry left behind, and one that is not a name at
/// all: `services::connect` was the call that resolved one.
///
/// Each is retired rather than renamed — `SYS_CONNECTION_JOIN` keeps number 76
/// and is a different call, addressed by handle, granting nothing. A word
/// boundary is what tells the two apart here.
const RETIRED_REGISTRY: &[&str] = &[
    "SYS_CONNECT",
    "SYS_LISTEN",
    "SYS_PIPE_OPEN",
    "SYS_PIPE_ID",
    "SYS_SOCKET_CREATE",
    "SharedToken",
    "services::connect",
];

/// Every other ABI name this project has retired: a deleted syscall, debug
/// action or inbox op code, whose *number* is retired with it and never
/// reused (`CLAUDE.md`, "Syscall ABI").
///
/// The number is what the rule protects and a number cannot be scanned for —
/// so the name is, and a name back in code is how a number gets reissued by
/// accident. Retired numbers themselves are recorded where they can be read
/// beside the live ones: the comments in `toyos-abi/src/syscall.rs` and
/// `toyos-abi/src/inbox.rs`, which this scan is blind to by construction
/// because it strips comments.
///
/// **A rename is not a retirement, and this table gained no row for one.**
/// `SYS_IO_URING_SETUP`/`SYS_IO_URING_ENTER` became `SYS_INBOX_SETUP`/
/// `SYS_INBOX_SUBMIT` on 2026-08-20 keeping numbers 89 and 90, the same
/// arguments and the same struct layouts, so nothing was deleted and no number
/// is protectable by forbidding the old spelling.
const RETIRED_ABI_NAMES: &[&str] = &[
    // Syscall 107. Nothing called it; a region's mappings go with its last
    // handle, so the handle is the whole of letting go.
    "SYS_SHM_UNMAP",
    // `SYS_DEBUG` actions 14 and 15. A total hides a leak of one kind behind
    // churn in another, and a breakdown in the kernel log is a reading no guest
    // test can see; every leak assertion in the estate is `CENSUS_KIND`.
    "CENSUS_TOTAL",
    "CENSUS_BREAKDOWN",
    // Inbox op code 2. No submitter anywhere: this kernel's watches are
    // one-shot and mio re-arms rather than cancels. Retired under both the
    // spelling it carried when it was deleted and the one a reintroduction
    // would write in today's vocabulary.
    "IORING_OP_POLL_REMOVE",
    "OP_POLL_REMOVE",
    "OP_CANCEL",
    // Inbox op code 4, `IORING_OP_CLOSE`: the one handle path that could not
    // obey the bad-handle policy, running under the ring's own lock. Same two
    // vocabularies.
    "IORING_OP_CLOSE",
    "OP_CLOSE",
];

/// Everything this repository compiles into the guest.
const GUEST_TREES: &[&str] =
    &["kernel/src", "toyos/src", "toyos-abi/src", "userland", "tests"];

/// `line` with its comment and its string literals removed.
///
/// What is left is the part that names things. Prose explaining what a deleted
/// call used to do is legal and worth keeping; a gravestone table mapping a
/// retired number to the string `"SYS_LISTEN"` is the point of the table.
fn code_only(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_string => {
                chars.next();
            }
            '"' => in_string = !in_string,
            '/' if !in_string && chars.peek() == Some(&'/') => break,
            _ if !in_string => out.push(c),
            _ => {}
        }
    }
    out
}

/// Whether `code` names `needle` as an identifier rather than as a fragment of
/// a longer one.
fn names(code: &str, needle: &str) -> bool {
    let bytes = code.as_bytes();
    let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    code.match_indices(needle).any(|(at, _)| {
        !word(at.checked_sub(1).and_then(|j| bytes.get(j)))
            && !word(bytes.get(at + needle.len()))
    })
}

/// `(file, line number)` for every place `needle` is named in code, over
/// [`GUEST_TREES`].
fn named_in_code(needle: &str) -> Vec<String> {
    let root = repo_root();
    let mut files = Vec::new();
    for tree in GUEST_TREES {
        rust_files(&root.join(tree), &mut files);
    }
    let mut found = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (n, line) in text.lines().enumerate() {
            if names(&code_only(line), needle) {
                found.push(format!("{}:{}", rel(&root, &path), n + 1));
            }
        }
    }
    found
}

/// Every line of `kernel/src` under a relative path, with its number.
fn kernel_lines() -> Vec<(String, usize, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("kernel/src"), &mut files);
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (n, line) in text.lines().enumerate() {
            out.push((rel(&root, &path), n + 1, line.to_string()));
        }
    }
    out
}

/// The three log macros and the function they all expand to.
const LOG_PRODUCERS: &[&str] = &["log!(", "alert!(", "boot_phase!(", "log::emit("];

/// Every `enable_bus_master(` site `kernel/src` holds, by file and count.
/// Arming DMA comes after a site's refusals (virtio parses first and disarms
/// on refusal; virtio_net once re-armed it before the parse), and the three
/// early-enabling MMIO drivers are `issues/isolation/`'s to fix, not a precedent.
const BUS_MASTER_SITES: &[(&str, usize)] = &[
    ("kernel/src/drivers/pci.rs", 1),
    ("kernel/src/drivers/virtio.rs", 1),
    ("kernel/src/drivers/hda.rs", 1),
    ("kernel/src/drivers/nvme.rs", 1),
    ("kernel/src/drivers/xhci/wait/boot.rs", 1),
];

/// The one place in `kernel/` a `!!!` may still appear, and how many times.
///
/// **Named by file and count rather than tested against the same line as a
/// `log!`.** A macro invocation is not a line — `rustfmt` puts a long one's
/// format string on its own — so "this line has `!!!` and a `log!` on it" is
/// defeated by a line break, and defeated silently. Every `!!!` in the tree is
/// listed here instead, so a new one is a red wherever it is written and
/// whatever it is written next to.
///
/// It writes raw bytes straight to the UART. They never enter the ring, so
/// `panic_console`'s deleted scan could not see them either and the record's
/// typed `Level` was never their business. Counted in occurrences of `!!!` and
/// not in lines, because it writes one at each end of its message.
///
/// **It was two.** `arch::idt::exceptions::debug_handler` put
/// `\n!!! DB TRAP !!!\n` out the port before it disarmed `DR7`, and the handler
/// went when `#DB` from Ring 3 became the ordinary Ring 3 fault it is. The gate
/// reds on a stale exemption as well as on a new marker, which is what made this
/// row part of that deletion rather than something to notice later.
const SENTINEL_ALLOWED: &[(&str, usize)] = &[
    // The two ends of `panic::last_words`' first line — `\n!!! <the dead end
    // this is> !!!` — written with the IDT possibly gone. Two whichever dead
    // end called it, because there is one writer of them.
    ("kernel/src/panic.rs", 2),
];

/// Every hand-written `Send`/`Sync` impl `kernel/src` holds, by file and count.
///
/// One of these stops the compiler re-deriving the bound, so a field added
/// later that is not `Send` — a raw pointer, an `Rc`, a `Cell` — keeps
/// compiling with nobody asked. Per file *and* per count, so an added impl
/// reds beside a permitted one and a deleted one reds its own stale row.
const AUTO_TRAIT_IMPLS: &[(&str, usize)] = &[
    ("kernel/src/completion/inbox.rs", 1),
    ("kernel/src/drivers/hda.rs", 1),
    ("kernel/src/drivers/panic_console/mod.rs", 2),
    ("kernel/src/drivers/virtio_console.rs", 1),
    ("kernel/src/drivers/virtio_sound.rs", 2),
    ("kernel/src/hw.rs", 1),
    ("kernel/src/mm/mmio.rs", 2),
    ("kernel/src/mm/region.rs", 2),
    ("kernel/src/pipe.rs", 1),
    ("kernel/src/process.rs", 1),
    ("kernel/src/sched/driver.rs", 2),
    ("kernel/src/symbols.rs", 2),
    ("kernel/src/trace.rs", 1),
];

/// Directories whose Rust is compiled for the guest or is not ours, by
/// repository-relative prefix. What is left is what runs on the development
/// host, and it is the only code that can reach a host binary: a ToyOS program
/// spawning `/bin/echo` is spawning a file out of its own image.
const NOT_HOST_CODE: &[&str] = &[
    "rust",
    "kernel/src",
    "toyos/src",
    "toyos-abi/src",
    "userland",
    "tests/toyos-rust-tests",
    "tests/testcases",
];

/// Every argument the host's code hands to `Command::new`, and what it names.
///
/// The dependency bar is Rust and QEMU (`CLAUDE.md`, "Dependencies"), and its
/// standing failures are declared rather than removed. A row here is that
/// declaration made checkable: an undeclared name is a red, and so is a name
/// spelled differently, which is what a scan for known-bad strings cannot say.
/// A non-literal argument is a row of its own, because a name assembled at run
/// time is a name this scan cannot read.
const HOST_SPAWNS: &[(&str, &str)] = &[
    ("\"cargo\"", "Rust's build driver, installed by rustup"),
    ("\"git\"", "the version control this repository is, and `REQUIRED` in src/main.rs"),
    ("\"rustc\"", "the Rust compiler, installed by rustup"),
    ("\"rustup\"", "how the toolchain is installed and linked, and `REQUIRED`"),
    ("\"qemu-system-x86_64\"", "QEMU, the other half of the bar, and `REQUIRED`"),
    (
        "\"gh\"",
        "GitHub's CLI, read-only, in `--merge-health` alone. Outside the bar and \
         declared by nothing else: no build, boot or gate reaches it",
    ),
    (
        "\"/sbin/newfs_msdos\"",
        "a macOS binary, and one of the standing failures CLAUDE.md declares. It \
         formats the FAT32 fixtures the crate's own reader is judged against; \
         issues/filesystem/fat32-suite-needs-macos-binaries.md is what removing it costs",
    ),
    (
        "\"/usr/bin/hdiutil\"",
        "the other one: newfs_msdos refuses a plain file, so the fixture is formatted \
         through a device node",
    ),
    ("std::env::current_exe().unwrap()", "this build system re-running itself under the lock"),
    (
        "x",
        "`./x` or `./x.py` in `rust/`, upstream's bootstrap. The `/bin/sh` half of the \
         Python standing failure — issues/build/python-and-cc-are-declared.md",
    ),
    (
        "cmd",
        "`run`'s parameter, and `src/toolchain.rs:1136` is its one caller: `rustup`. A \
         second caller is a name this scan cannot read and a row it must gain",
    ),
    ("env!(\"CARGO_BIN_EXE_toyos-cc\")", "our own compiler, built by cargo for its own tests"),
    ("env!(\"CARGO_BIN_EXE_toyos-ld\")", "our own linker, the same way"),
];

/// Every file `git` tracks whose bytes are not text, with the digest of what is
/// committed and where its terms are recorded.
///
/// A third column of `NOTICE` means that file carries this same digest, so the
/// obligation and the bytes cannot drift apart; anything else names the file
/// that carries the attribution, or says why it is ours.
const COMMITTED_BINARIES: &[(&str, &str, &str)] = &[
    (
        "assets/DOOM1.WAD",
        "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771",
        "NOTICE",
    ),
    (
        "assets/JetBrainsMono-Regular.ttf",
        "e6fd0d7e91550b3ed2b735d4312474362c4716edc4fc0577a0f61ed782d5aed1",
        "NOTICE",
    ),
    (
        "assets/soundfont.sf2",
        "89a13a5c907b5cc83c15679e07e6dcb06fd72102937e092dc4a582f1aa5905c3",
        "NOTICE",
    ),
    (
        "assets/wallpaper.jpg",
        "b6f0c89bf966cfb458333b280614f0c7723615e42e340b9d43a760a64fe05976",
        "ours: `cargo run -- --regen-wallpaper` writes it from src/wallpaper.rs",
    ),
    (
        "doom.jpg",
        "ae22f71dc732580bd4f789937c9fe564969029413fc2092f27bdae8d1ceaf8e3",
        "ours: a screenshot of this system running, in README.md",
    ),
    (
        "first-boot.jpg",
        "41a65f4bf1f752bcc9da717e3c8f7f776bfbc214b2354b11a3c1e0278a9be22e",
        "ours: the T14's first boot photographed, in README.md",
    ),
    (
        "kernel/src/drivers/panic_console/font8x16.bin",
        "e1bea9791e07a0e2509196c6cb4563d44cafca1f19b0ef660319b0fa53546a3e",
        "NOTICE",
    ),
    (
        "ovmf/DEBUGX64_OVMF.fd",
        "800ff5af1220d1232d4da7173ccddbb74a9217600bd8935903d9d534801778b4",
        "NOTICE",
    ),
    (
        "ovmf/OVMF_CODE-pure-efi.fd",
        "9de33971d47958f42af86584b502f83256120b2482e4f7ed14db32fd68e92922",
        "NOTICE",
    ),
    (
        "ovmf/OVMF_VARS-pure-efi.fd",
        "c653de93db67e4f2213a35598efb379a13ef4a12c241e003699d4d7afd193635",
        "NOTICE",
    ),
    (
        "toyos-elf/tests/fixtures/toyos-ld-headers.bin",
        "6243d543a15941133514c1a8a24c79d118060caeae7e985870a67d9fc3021354",
        "ours: the first 4096 bytes of a toyos-ld output (toyos-elf/tests/real.rs)",
    ),
    (
        "toyos-symbols/tests/fixtures/input-test.bin",
        "6a08f75ee01bdbd1e77c9b3affd6185e981d86995da432c90ed107676f08eb83",
        "ours: a ToyOS binary this build produced (toyos-symbols/tests/real.rs)",
    ),
];

/// `text` looks like a binary file by git's own heuristic: a NUL in the first
/// 8000 bytes.
#[cfg(test)]
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

/// Every `.rs` file under the repository that runs on the host.
#[cfg(test)]
fn host_files() -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            let at = rel(root, &path);
            if NOT_HOST_CODE.iter().any(|skip| at == *skip || at.starts_with(&format!("{skip}/")))
            {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

/// The argument text of every `Command::new(` `text` names in code, verbatim.
///
/// Verbatim is the point: a literal and the expression that computes one are
/// different rows, because only the first names a binary this gate can read.
/// A call written inside a string literal or a comment is not a call — this
/// file's own refusal messages and its case table both spell one out.
#[cfg(test)]
fn spawn_arguments(text: &str) -> Vec<String> {
    const OPEN: &str = "Command::new(";
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut in_string = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => i += 2,
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => break,
            _ if !in_string && bytes[i..].starts_with(OPEN.as_bytes()) => {
                i += OPEN.len();
                let start = i;
                let mut depth = 1usize;
                let mut inner = false;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if inner => i += 1,
                        b'"' => inner = !inner,
                        b'(' if !inner => depth += 1,
                        b')' if !inner => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                let arg = String::from_utf8_lossy(&bytes[start..i.min(bytes.len())]);
                out.push(arg.trim().to_string());
                i = (i + 1).min(bytes.len());
            }
            _ => i += 1,
        }
    }
    out
}

/// `bytes` as lower-case hex SHA-256, the spelling `NOTICE` records.
#[cfg(test)]
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    /// **Two clauses, one rule each, and neither is checkable any other way.**
    ///
    /// The first: the NMI handler must not log. It would reenter its own CPU's
    /// log shard — the reservation is sound only because the CPU that owns the
    /// shard has `IF` and `TF` masked through publication, and an NMI is the one
    /// interrupt that ignores `IF`. `dump_nmi_probe` is what makes the handler
    /// *useful*; this is what keeps it silent.
    ///
    /// The second: no log producer in `kernel/` carries `!!!` in its format
    /// string. `panic_console::has_alert` used to scan every display row for
    /// three exclamation marks, and its own comment enumerated the messages that
    /// happened to match; the panel reads `Level` off the record now, so a `!!!`
    /// put back into a message would be a marker marking nothing — a second,
    /// silent alert channel beside the typed one. The two `!!!` still in
    /// `kernel/` write raw bytes straight to the UART, never enter the ring, and
    /// were never `has_alert`'s business either.
    #[test]
    fn nmi_does_not_log() {
        let lines = kernel_lines();
        assert!(
            lines.iter().any(|(file, _, _)| file == "kernel/src/arch/idt/nmi.rs"),
            "the NMI handler moved: this gate is scanning a file that is not there"
        );

        let silent: Vec<_> = lines
            .iter()
            .filter(|(file, _, line)| {
                file == "kernel/src/arch/idt/nmi.rs"
                    && LOG_PRODUCERS.iter().any(|p| code_only(line).contains(p))
            })
            .map(|(file, n, line)| format!("{file}:{n}: {}", line.trim()))
            .collect();
        assert!(
            silent.is_empty(),
            "the NMI handler logs, and it reenters its own CPU's shard to do it:\n{}",
            silent.join("\n")
        );

        let mut found: Vec<(String, usize)> = Vec::new();
        for (file, _, line) in &lines {
            let n = line.matches("!!!").count();
            if n == 0 {
                continue;
            }
            match found.last_mut() {
                Some((last, count)) if last == file => *count += n,
                _ => found.push((file.clone(), n)),
            }
        }
        let mut complaints = Vec::new();
        for (file, count) in &found {
            match SENTINEL_ALLOWED.iter().find(|(allowed, _)| allowed == file) {
                Some((_, want)) if want == count => {}
                Some((_, want)) => complaints.push(format!(
                    "{file} has {count} `!!!` where this gate exempts {want} raw-UART ones"
                )),
                None => complaints.push(format!("{file} has {count} `!!!`")),
            }
        }
        for (file, want) in SENTINEL_ALLOWED {
            if !found.iter().any(|(f, _)| f == file) {
                complaints.push(format!(
                    "{file} no longer has the {want} raw-UART `!!!` this gate exempts, so the \
                     exemption is stale"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "the `!!!` sentinel is deleted: the panel paints a red row from `Level::Alert` and \
             reads nothing out of the text, so a marker put back into a message marks \
             nothing.\n{}",
            complaints.join("\n")
        );
    }

    /// **A `Send`/`Sync` the compiler can derive is the compiler's to derive.**
    /// A hand-written one is a standing exemption from that re-derivation, so
    /// every one the kernel keeps is named here with the count its file holds.
    #[test]
    fn every_hand_written_auto_trait_impl_is_declared() {
        let mut found: Vec<(String, usize)> = Vec::new();
        for (file, _, line) in kernel_lines() {
            let code = code_only(&line);
            let code = code.trim_start();
            if !code.starts_with("unsafe impl Send for")
                && !code.starts_with("unsafe impl Sync for")
            {
                continue;
            }
            match found.last_mut() {
                Some((last, count)) if *last == file => *count += 1,
                _ => found.push((file, 1)),
            }
        }
        assert!(
            !found.is_empty(),
            "the scan found no hand-written impl at all, so it is reading no tree"
        );

        let mut complaints = Vec::new();
        for (file, count) in &found {
            match AUTO_TRAIT_IMPLS.iter().find(|(f, _)| f == file) {
                Some((_, want)) if want == count => {}
                Some((_, want)) => complaints.push(format!(
                    "{file} hand-writes {count} `Send`/`Sync` impls where this table declares \
                     {want}"
                )),
                None => complaints.push(format!(
                    "{file} hand-writes {count} `Send`/`Sync` impls and is not in this table"
                )),
            }
        }
        for (file, want) in AUTO_TRAIT_IMPLS {
            if !found.iter().any(|(f, _)| f == file) {
                complaints.push(format!(
                    "{file} no longer hand-writes the {want} impls declared here, so the row is \
                     stale"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "a hand-written `Send`/`Sync` is a bound the compiler stops checking on every later \
             field, so each one is a row somebody wrote on purpose.\n{}",
            complaints.join("\n"),
        );
    }

    #[test]
    fn nothing_in_the_kernel_counts_a_reference_by_hand() {
        let mut complaints = Vec::new();
        for ban in BANS {
            for (file, count) in occurrences(ban.needle) {
                let allowed = ban
                    .allowed
                    .iter()
                    .find(|(f, _)| *f == file)
                    .map_or(0, |(_, n)| *n);
                if count > allowed {
                    complaints.push(format!(
                        "{file}: {count} × `{}`, {allowed} allowed — {}",
                        ban.needle, ban.why,
                    ));
                }
            }
        }
        assert!(complaints.is_empty(), "{}", complaints.join("\n"));
    }

    /// Both directions: a resurrected early enable reds, and so does a stale row.
    #[test]
    fn bus_mastering_is_armed_at_exactly_the_declared_sites() {
        let found = occurrences("enable_bus_master(");
        for (file, n) in &found {
            let allowed = BUS_MASTER_SITES.iter().find(|(f, _)| f == file).map_or(0, |(_, c)| *c);
            assert_eq!(
                *n, allowed,
                "{file}: {n} × `enable_bus_master(`, {allowed} declared — arming DMA is a declared decision, and it comes after the site's refusals"
            );
        }
        for (file, n) in BUS_MASTER_SITES {
            let count = found.iter().find(|(f, _)| f == *file).map_or(0, |(_, c)| *c);
            assert_eq!(
                count, *n,
                "{file} is declared {n} × `enable_bus_master(` and has {count} — a stale row is a permission nobody re-argued"
            );
        }
    }

    /// An exception that has gone stale is a permission nobody re-argued.
    #[test]
    fn every_named_exception_is_still_there() {
        for ban in BANS {
            let found = occurrences(ban.needle);
            for (file, allowed) in ban.allowed {
                let count = found.iter().find(|(f, _)| f == file).map_or(0, |(_, n)| *n);
                assert_eq!(
                    count, *allowed,
                    "{file} is allowed {allowed} × `{}` and has {count}. \
                     An exception is a decision, so it goes when its call site does.",
                    ban.needle,
                );
            }
        }
    }

    /// **There is no global registry.** A name a process could present and have
    /// resolved for it is the thing this architecture deletes, so its
    /// identifiers may not be reachable from any code the guest compiles.
    #[test]
    fn no_name_resolves_through_a_registry_any_more() {
        let mut complaints = Vec::new();
        for needle in RETIRED_REGISTRY {
            for at in named_in_code(needle) {
                complaints.push(format!("{at}: names `{needle}`"));
            }
        }
        assert!(
            complaints.is_empty(),
            "the registry is deleted, and these still name it:\n  {}",
            complaints.join("\n  "),
        );
    }

    /// **A retired ABI number is never reused**, and the name is the only part
    /// of it a scan can hold on to. A retired name back in guest-compiled code
    /// is either the number coming back or a new call wearing a dead one's
    /// identity, and the two are indistinguishable from the outside.
    #[test]
    fn a_retired_abi_name_is_gone_from_the_code() {
        let mut complaints = Vec::new();
        for needle in RETIRED_ABI_NAMES {
            for at in named_in_code(needle) {
                complaints.push(format!("{at}: names `{needle}`"));
            }
        }
        assert!(
            complaints.is_empty(),
            "these names are retired and their numbers with them:\n  {}",
            complaints.join("\n  "),
        );
    }

    /// What the scan above can and cannot see, stated as cases, because a
    /// well-formed tree exercises none of them.
    #[test]
    fn the_registry_scan_reads_code_and_not_prose() {
        assert!(names(&code_only("    let x = syscall(SYS_LISTEN, 0);"), "SYS_LISTEN"));
        assert!(names(&code_only("pub const SYS_PIPE_ID: u64 = 70;"), "SYS_PIPE_ID"));
        assert!(!names(&code_only("/// `SYS_LISTEN` used to register a name."), "SYS_LISTEN"));
        assert!(!names(&code_only("    // SYS_PIPE_ID was 70"), "SYS_PIPE_ID"));
        assert!(!names(&code_only("    85 => \"SYS_LISTEN\","), "SYS_LISTEN"));
        // The live call keeps the retired one's number and must not be read as
        // it: this is the whole reason the match is on a word boundary.
        assert!(!names(&code_only("SYS_CONNECTION_JOIN => join(a, b),"), "SYS_CONNECT"));
        assert!(names(&code_only("SYS_CONNECT => connect(a),"), "SYS_CONNECT"));
        // And the walk reaches real code: a live name it is capable of finding
        // must actually be found.
        assert!(
            !named_in_code("SYS_CONNECTION_JOIN").is_empty(),
            "the scan found no `SYS_CONNECTION_JOIN` in code, so it is not reading the guest trees",
        );
    }

    /// The scan has teeth only if it can find anything: this file names every
    /// banned identifier in its own table, and the walk must not be looking at
    /// a tree where none of them can occur.
    #[test]
    fn the_scan_reaches_the_trees_it_claims_to() {
        let root = repo_root();
        for tree in TREES {
            let mut files = Vec::new();
            rust_files(&root.join(tree), &mut files);
            assert!(!files.is_empty(), "{tree} has no .rs files — the walk is looking elsewhere");
        }
        assert!(
            !occurrences("mem::forget").is_empty(),
            "the permitted `mem::forget` call sites are not being found, so one \
             more would not be either",
        );
    }

    /// A page a process can write while the kernel is inside it never becomes a
    /// slice, and never an exclusive reference. Two files, named rather than
    /// walked: both spellings are ordinary elsewhere.
    ///
    /// A *shared* reference is not banned: `Ring::header` soundly takes one over
    /// the same page, its whole subject being an `AtomicU32`.
    #[test]
    fn no_slice_or_exclusive_reference_is_built_over_a_mapped_page() {
        const OVER_A_MAPPING: &[&str] = &["toyos-abi/src/ring.rs", "kernel/src/user_ptr.rs"];
        const SLICES: &[&str] =
            &["from_raw_parts", "from_raw_parts_mut", "from_ptr_range", "from_ptr_range_mut"];
        const EXCLUSIVE: &str = "&mut *";
        let root = repo_root();
        let mut complaints = Vec::new();
        for file in OVER_A_MAPPING {
            let text = std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|e| panic!("{file}: {e} — the scan is looking elsewhere"));
            for (n, line) in text.lines().enumerate() {
                let code = code_only(line);
                for spelling in SLICES {
                    if names(&code, spelling) {
                        complaints.push(format!("{file}:{}: `{spelling}`", n + 1));
                    }
                }
                if code.contains(EXCLUSIVE) {
                    complaints.push(format!("{file}:{}: `{EXCLUSIVE}`", n + 1));
                }
            }
        }
        assert!(
            complaints.is_empty(),
            "a slice or an exclusive reference over a page a process can write \
             claims an exclusivity the mapping does not give:\n{}",
            complaints.join("\n"),
        );
    }

    /// **The dependency bar, made checkable at the one place a host binary can
    /// be reached from.** Rust and QEMU are the whole of what this project may
    /// need; the standing failures are declared rather than removed, and a row
    /// here is that declaration written where something reads it.
    ///
    /// Keyed on the argument text rather than on a set of known-bad names: a
    /// scan for `fsck_msdos` passes the day the call is spelled
    /// `/sbin/fsck_msdos`, and says nothing at all about the tool that arrives
    /// next. An argument that is not a literal is a row of its own, because a
    /// name assembled at run time is a name this scan cannot read.
    #[test]
    fn every_binary_the_host_runs_is_declared() {
        let root = repo_root();
        let mut found: Vec<(String, String)> = Vec::new();
        for path in host_files() {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for (n, line) in text.lines().enumerate() {
                for arg in spawn_arguments(line) {
                    found.push((arg, format!("{}:{}", rel(&root, &path), n + 1)));
                }
            }
        }
        assert!(
            found.iter().any(|(arg, _)| arg == "\"qemu-system-x86_64\""),
            "the walk did not find the QEMU launch, so it is reading no host tree"
        );

        let mut complaints = Vec::new();
        for (arg, at) in &found {
            if !HOST_SPAWNS.iter().any(|(a, _)| a == arg) {
                complaints.push(format!("{at}: `Command::new({arg})`, which nothing declares"));
            }
        }
        for (arg, why) in HOST_SPAWNS {
            if !found.iter().any(|(a, _)| a == arg) {
                complaints.push(format!(
                    "nothing runs `Command::new({arg})` any more, so the row saying it is {why} \
                     is a permission nobody re-argued"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "only Rust and QEMU are in the bar, and what is outside it is declared:\n{}",
            complaints.join("\n"),
        );
    }

    /// What the walk can and cannot read, stated as cases.
    #[test]
    fn the_spawn_scan_reads_the_argument_and_not_the_call() {
        assert_eq!(spawn_arguments("Command::new(\"git\")"), ["\"git\""]);
        assert_eq!(spawn_arguments("  let c = Command::new(&exe);"), ["&exe"]);
        assert_eq!(
            spawn_arguments("Command::new(env!(\"CARGO_BIN_EXE_toyos-ld\"))"),
            ["env!(\"CARGO_BIN_EXE_toyos-ld\")"]
        );
        let call = "Command::new(format!(\"/bin/{cmd}\"))";
        assert_eq!(spawn_arguments(call), ["format!(\"/bin/{cmd}\")"]);
        assert_eq!(spawn_arguments("Command::new(\"a\"); Command::new(\"b\")"), ["\"a\"", "\"b\""]);
        assert!(spawn_arguments("let x = 1;").is_empty());
    }

    /// **Every committed binary file is a file somebody judged.** `NOTICE`
    /// names each one's upstream, licence and digest; nothing read it, so a new
    /// one arrived unremarked and a changed one changed silently.
    ///
    /// The digest is taken from the bytes and held against both the row and, for
    /// a third-party file, `NOTICE`'s own text — so the obligation and the
    /// bytes it is an obligation about cannot drift apart.
    #[test]
    fn every_committed_binary_file_is_declared() {
        let root = repo_root();
        let out = std::process::Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|e| panic!("git ls-files: {e}"));
        assert!(out.status.success(), "git ls-files failed");
        let listing = String::from_utf8(out.stdout).expect("git ls-files is not UTF-8");

        let mut found: Vec<(String, String)> = Vec::new();
        for name in listing.split('\0').filter(|s| !s.is_empty()) {
            let Ok(bytes) = std::fs::read(root.join(name)) else { continue };
            if !is_binary(&bytes) {
                continue;
            }
            found.push((name.to_string(), digest(&bytes)));
        }
        assert!(
            found.iter().any(|(name, _)| name == "assets/DOOM1.WAD"),
            "the walk found no DOOM1.WAD, so it is not reading the tracked tree"
        );

        let notice = std::fs::read_to_string(root.join("NOTICE")).expect("NOTICE");
        let mut complaints = Vec::new();
        for (name, sha) in &found {
            match COMMITTED_BINARIES.iter().find(|(f, _, _)| f == name) {
                None => complaints
                    .push(format!("{name} is committed, is not text, and nothing declares it")),
                Some((_, want, _)) if want != sha => complaints.push(format!(
                    "{name} is sha256 {sha} where it is declared {want}"
                )),
                Some((_, _, "NOTICE")) if !notice.contains(sha.as_str()) => complaints
                    .push(format!("{name} is sha256 {sha}, which NOTICE does not carry")),
                Some(_) => {}
            }
        }
        for (name, _, where_from) in COMMITTED_BINARIES {
            if !found.iter().any(|(f, _)| f == name) {
                complaints.push(format!(
                    "{name} is declared here ({where_from}) and is no longer a committed binary"
                ));
            }
        }
        assert!(
            complaints.is_empty(),
            "a committed binary is a file somebody had to judge, and NOTICE is where the \
             judgement is:\n{}",
            complaints.join("\n"),
        );
    }
}
