use toyos_phys::Phys;

#[test]
fn an_address_off_its_alignment_or_past_48_bits_is_no_address() {
    assert_eq!(Phys::<6>::new(0x40).map(Phys::get), Some(0x40));
    assert_eq!(Phys::<6>::new(0x20), None);
    assert_eq!(Phys::<8>::new(0x100).map(Phys::get), Some(0x100));
    assert_eq!(Phys::<8>::new(0x80), None);
    assert_eq!(Phys::<12>::new(0xfff), None);
    assert_eq!(Phys::<12>::new(0x800), None);
    assert_eq!(Phys::<16>::new(0x8000), None);
    assert_eq!(Phys::<21>::new(0x10_0000), None);
    assert_eq!(Phys::<21>::new(0x20_0000).map(Phys::get), Some(0x20_0000));
    assert_eq!(Phys::<12>::new(1 << 48), None);
    assert_eq!(Phys::<12>::new((1 << 48) - 0x1000).map(Phys::get), Some((1 << 48) - 0x1000));
    assert_eq!(Phys::<16>::new((1 << 48) - 0x1_0000).map(Phys::get), Some((1 << 48) - 0x1_0000));
    assert_eq!(Phys::<0>::new(u64::MAX), None);
    assert_eq!(Phys::<0>::new((1 << 48) - 1).map(Phys::get), Some((1 << 48) - 1));
}

#[test]
fn a_words_address_field_is_its_bits_from_the_alignment_to_47() {
    assert_eq!(Phys::<12>::of(u64::MAX).get(), 0x0000_ffff_ffff_f000);
    assert_eq!(Phys::<12>::of(0x8060_0000_4020_3fff).get(), 0x4020_3000);
    assert_eq!(Phys::<21>::of(u64::MAX).get(), 0x0000_ffff_ffe0_0000);
    assert_eq!(Phys::<12>::of(0x1000), Phys::new(0x1000).unwrap());
}
