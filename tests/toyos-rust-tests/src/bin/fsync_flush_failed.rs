//! Two fsyncs under a device that refuses SYNCHRONIZE CACHE (`usb-flush-fails`):
//! both must refuse. The second returning success is the F5 lie — the failed
//! device commit forgotten because the file's own flush had already settled.
//! `fsync_failed_commit` in `tests/common/volumes.rs` boots and judges this.

use std::fs::File;
use std::io::Write;

const PATH: &str = "/log/f5-flush-failed.bin";

fn main() {
    let mut f = File::create(PATH).expect("create on /log");
    f.write_all(&[0xC3u8; 2 * 4096 + 33]).expect("write");

    let first = f.sync_all();
    println!("first fsync: {first:?}");
    assert!(first.is_err(), "fsync reported success while the device refused its cache flush");

    let second = f.sync_all();
    println!("second fsync: {second:?}");
    assert!(
        second.is_err(),
        "the second fsync reported success without reaching the device — the failed commit \
         was forgotten"
    );
    println!("both fsyncs refused: the failed device commit stays owed");
}
