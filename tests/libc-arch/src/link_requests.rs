//! The buffer `readlink` refuses before the kernel is asked: none, and one
//! longer than `{SSIZE_MAX}`.

use crate::linkreq;

#[test]
fn readlink_refuses_an_empty_buffer_and_one_no_ssize_t_counts() {
    for size in [1, 4, 4096, isize::MAX as usize] {
        assert_eq!(linkreq::target_len(size), Some(size), "{size}");
    }
    for size in [0, isize::MAX as usize + 1, usize::MAX] {
        assert_eq!(linkreq::target_len(size), None, "{size}");
    }
}
