//! A thread's vector registers are its own: a new thread starts with every one
//! above `xmm0`-`xmm15` zero, and what it loads into all of them survives
//! being switched out for threads holding other values, whatever the kernel
//! wrote on the thread's own kernel stack in between — and survives a return
//! to a CPU whose registers still hold what it had there before it ran
//! elsewhere.
//!
//! More threads than CPUs, each spinning with its registers loaded, so every
//! CPU is time-shared and each switch puts a thread with other values on it.
//! The spin stops every so often for an object `fstat`, which the kernel runs
//! on the thread's own kernel stack, below the frame of the switch that
//! preempts the spin. A CPU whose save skips a component unmodified since its
//! restore from the same address (SDM Vol. 1 §13.6) keeps it only where
//! nothing wrote the thread's save area in between.

#[path = "../arch/vector.rs"]
mod vector;

/// Rounds per thread, each with a pattern of its own, so a value left from
/// the thread's own previous round is caught as surely as another thread's.
const ROUNDS: u64 = 3;

/// Bursts of the hold loop, each ending in an object `fstat`: past a quantum on the
/// T14 and under TCG alike.
const BURSTS: u64 = 1_000;

/// The bytes `thread` loads in `round`: no two threads or rounds alike.
fn pattern(thread: u64, round: u64, len: usize) -> Vec<u8> {
    (0..len.div_ceil(8) as u64)
        .flat_map(|word| {
            // splitmix64's finaliser over a distinct seed per word.
            let mut z = (thread << 40 | round << 32 | word).wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(len)
        .collect()
}

fn main() {
    let width = vector::width();
    let threads = 2 * toyos_abi::syscall::cpu_count() as u64 + 1;
    let workers: Vec<_> = (0..threads)
        .map(|thread| {
            std::thread::spawn(move || {
                let fresh = vector::read(width);
                if let Some(register) = width.first_inherited(&fresh) {
                    panic!("thread {thread} started with {register} holding a value it never loaded");
                }
                for round in 0..ROUNDS {
                    let want = pattern(thread, round, width.bytes());
                    let got = vector::hold(width, &want, BURSTS);
                    if let Some(register) = width.first_difference(&got, &want) {
                        panic!("thread {thread} round {round}: {register} changed while it was switched out");
                    }
                }
            })
        })
        .collect();
    for (thread, worker) in workers.into_iter().enumerate() {
        assert!(worker.join().is_ok(), "thread {thread} found a vector register not its own");
    }
    let trips = away_and_back(width);
    println!("vector_state: {threads} threads each held {width:?} through {ROUNDS} rounds; {trips}");
}

/// Trips from one CPU to another and back, each a pair of files no other trip
/// loads.
const TRIPS: u64 = 16;

/// The main thread leaves its registers on a CPU that then idles, changes them
/// on another, and comes back: what it reads there is what it loaded last.
fn away_and_back(width: vector::Width) -> String {
    let cpus = toyos_abi::syscall::cpu_count();
    if cpus < 2 {
        return "no second CPU to go away to".to_string();
    }
    let (home, away) = (cpus - 1, cpus - 2);
    for trip in 0..TRIPS {
        let first = pattern(u64::MAX, 2 * trip, width.bytes());
        let second = pattern(u64::MAX, 2 * trip + 1, width.bytes());
        let (got, ran) = vector::away_and_back(width, &first, &second, home, away);
        let wanted = [u64::from(home), u64::from(away), u64::from(home)];
        assert_eq!(ran, wanted, "trip {trip} ran its legs on CPUs {ran:?}, not {wanted:?}");
        if let Some(register) = width.first_difference(&got, &second) {
            let held = if got == first { "the file it had left there" } else { "neither file" };
            panic!("trip {trip}: back on CPU {home} from CPU {away}, {register} held {held}");
        }
    }
    format!("{TRIPS} trips from CPU {home} to {away} and back kept what each loaded last")
}
