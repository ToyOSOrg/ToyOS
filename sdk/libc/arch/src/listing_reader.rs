//! What `readdir` makes of a name: one `d_name` holds, or one it refuses.

use crate::listing;

#[test]
fn a_name_fits_d_name_with_its_nul_or_is_refused() {
    for len in [0, 1, 254, 255] {
        let name = vec![b'n'; len];
        let held = listing::d_name(&name).unwrap_or_else(|| panic!("a {len}-byte name was refused"));
        assert_eq!(&held[..len], &name[..]);
        assert!(held[len..].iter().all(|&b| b == 0), "a {len}-byte name is not NUL-terminated");
    }
    for len in [256, 257, 765] {
        assert!(listing::d_name(&vec![b'n'; len]).is_none(), "a {len}-byte name was held");
    }
}
