//! Panic at once. The binary a swap's negative control puts in a running
//! service's place: a replacement that does not start, which init must answer
//! by starting the binary it replaced again.

fn main() {
    panic!("swap_crash: this binary ends the instant it starts");
}
