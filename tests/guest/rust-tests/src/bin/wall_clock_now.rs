//! What `SYS_CLOCK_EPOCH` answers, from inside the machine.
//!
//! What it asserts itself is only what holds on *every* machine, because it
//! runs on four of them: the shared boot, and the three whose clocks are staged
//! broken. A machine with no wall clock is not a failure here — printing that
//! it has none is the answer the host is checking for.

const CALLS: u32 = 1000;

fn main() {
    let Some(epoch) = toyos::system::clock_epoch() else {
        println!("wall-clock: no epoch");
        return;
    };
    println!("wall-clock: epoch={epoch}");

    // What `std` makes of the same clock. The host puts this print against the
    // instant it staged in the RTC, which is the one reading from outside.
    let std_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("std put the wall clock before the epoch")
        .as_secs();
    println!("wall-clock: std_epoch={std_epoch}");

    let mut last = 0;
    for _ in 0..CALLS {
        last = toyos::system::clock_epoch().expect("the clock answered once and then stopped");
    }
    println!("wall-clock: {CALLS} calls, last={last}");

    // Monotonic-plus-offset cannot go backwards inside a boot, and std's
    // reading sits between the two the kernel gave either side of it: order,
    // with no margin in it.
    assert!(last >= epoch, "the wall clock went backwards: {epoch} then {last}");
    assert!(
        (epoch..=last).contains(&std_epoch),
        "std's SystemTime::now says {std_epoch}, outside the {epoch}..={last} SYS_CLOCK_EPOCH \
         answered either side of it",
    );
}
