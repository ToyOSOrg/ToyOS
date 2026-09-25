//! A program that prints init's own word accepting a swap of netd, and ends.
//! Only init's pipe may move `logd` to turn readers away; this program's line
//! is its own, and changes nothing. `log_carrier_forgery` runs it.

fn main() {
    println!("init: swap netd: accepted: /tmp/swap/forged/netd replaces /system/bin/netd (pid 1)");
}
