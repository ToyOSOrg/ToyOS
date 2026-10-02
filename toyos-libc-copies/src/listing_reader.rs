//! What `opendir`, `readdir` and `dladdr` make of the kernel's answers: a
//! whole answer however often it grows between two asks, and a name `d_name`
//! holds or `readdir` refuses.

use crate::listing;

/// A call answering `sizes` in turn, each the length its answer then needs,
/// writing it only into a buffer that holds it.
fn growing(sizes: &[usize]) -> impl FnMut(&mut [u8]) -> Result<usize, ()> + '_ {
    let mut asked = 0;
    move |buf| {
        let need = sizes[asked.min(sizes.len() - 1)];
        asked += 1;
        if need <= buf.len() {
            buf[..need].iter_mut().enumerate().for_each(|(i, b)| *b = i as u8);
        }
        Ok(need)
    }
}

#[test]
fn an_answer_is_asked_again_until_it_fits() {
    // The first ask is of an empty buffer; each growth is met at the next.
    for (sizes, want) in [
        (&[0][..], 0),
        (&[5], 5),
        (&[5, 9], 9),
        (&[5, 9, 4000, 70_000], 70_000),
        (&[70_000, 3], 3),
    ] {
        let whole = listing::whole(growing(sizes)).unwrap();
        assert_eq!(whole.len(), want, "{sizes:?}");
        assert!(whole.iter().enumerate().all(|(i, &b)| b == i as u8), "{sizes:?}");
    }
    assert_eq!(listing::whole(|_| Err::<usize, _>(7)), Err(7));
}

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
