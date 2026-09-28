//! `--frame-check`: what doom's renderer draws, as one number.
//!
//! doomgeneric plays `demo1` from `DOOM1.WAD` as a timedemo — one game tic per
//! frame, as fast as the machine renders, with no input and no sound — and each
//! tic's last frame is folded into a hash. A demo replays identically on every
//! machine, so the hash is a function of the WAD and of the C that renders it,
//! and of nothing else: a compiler that changes what doom draws changes it.
//!
//! **The last frame of each tic, never every frame.** A screen wipe draws as
//! many frames inside one tic as the wall clock lets it; the frame it ends on is
//! the wiped-to screen, and nothing before it is decided by the game.
//!
//! Driven by `tests/toyos-rust-tests/src/bin/doom_frames.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The tics hashed. Well inside `demo1`, which ends the timedemo when it does.
pub const TICS: u32 = 700;

/// Whether this process is the frame check rather than the game.
static CHECKING: AtomicBool = AtomicBool::new(false);

struct Fold {
    /// The frame drawn last and the tic it was drawn at, which a later frame
    /// in the same tic replaces.
    pending: Option<(i32, u64)>,
    /// Every finished tic's frame, folded in order.
    hash: u64,
    tics: u32,
    frames: u32,
}

static FOLD: Mutex<Fold> = Mutex::new(Fold { pending: None, hash: FNV_OFFSET, tics: 0, frames: 0 });

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv(mut hash: u64, words: impl IntoIterator<Item = u32>) -> u64 {
    for word in words {
        hash = (hash ^ u64::from(word)).wrapping_mul(FNV_PRIME);
    }
    hash
}

unsafe extern "C" {
    /// The tic the game has run to, `d_loop.c`'s.
    static gametic: i32;
}

/// Play the demo and print its hash; never returns, because doomgeneric has no
/// way back out of its loop but `exit`.
pub fn frame_check() -> ! {
    CHECKING.store(true, Ordering::Relaxed);
    // `-config` names a file no image carries, so every setting is doom's
    // default and nothing a previous run saved can move a pixel.
    let argv: Vec<*const u8> = vec![
        c"doom".as_ptr().cast(),
        c"-iwad".as_ptr().cast(),
        c"/system/share/doom1.wad".as_ptr().cast(),
        c"-config".as_ptr().cast(),
        c"/system/share/no-such-doom.cfg".as_ptr().cast(),
        c"-nosound".as_ptr().cast(),
        c"-timedemo".as_ptr().cast(),
        c"demo1".as_ptr().cast(),
    ];
    let argv = argv.leak();
    unsafe {
        crate::doomgeneric_Create(argv.len() as i32, argv.as_ptr());
        loop {
            crate::doomgeneric_Tick();
        }
    }
}

/// `DG_DrawFrame`'s half of the check: fold the frame `screen` holds, and exit
/// once [`TICS`] tics are in.
pub fn drawn(screen: &[u32]) {
    if !CHECKING.load(Ordering::Relaxed) {
        return;
    }
    let tic = unsafe { gametic };
    let this = fnv(FNV_OFFSET, screen.iter().copied());
    let mut fold = FOLD.lock().expect("the frame fold");
    fold.frames += 1;
    match fold.pending {
        Some((at, _)) if at == tic => {}
        Some((at, last)) => {
            fold.hash = fnv(fold.hash, [at as u32, last as u32, (last >> 32) as u32]);
            fold.tics += 1;
        }
        None => {}
    }
    fold.pending = Some((tic, this));
    if fold.tics == TICS {
        println!("[frame-check] tics={} frames={} hash={:016x}", fold.tics, fold.frames, fold.hash);
        std::process::exit(0);
    }
}
