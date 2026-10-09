extern crate std;

use std::vec::Vec;

use super::*;

/// The bytes of a hex dump as RFC 8439 prints one, without its offsets.
fn hex(dump: &str) -> Vec<u8> {
    dump.split_whitespace().map(|byte| u8::from_str_radix(byte, 16).expect("a hex byte")).collect()
}

fn block_of(key: &[u8], counter: u32, nonce: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    chacha::block(key.try_into().expect("a 32-byte key"), counter, nonce.try_into().expect("a 12-byte nonce"), &mut out);
    out
}

/// RFC 8439 §2.1.1.
#[test]
fn the_quarter_round_is_rfc_8439s() {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&[0x1111_1111, 0x0102_0304, 0x9b8d_6f43, 0x0123_4567]);
    chacha::quarter_round(&mut state, 0, 1, 2, 3);
    assert_eq!(state[..4], [0xea2a_92f4, 0xcb1c_f8ce, 0x4581_472e, 0x5881_c4bb]);
}

/// RFC 8439 §2.2.1.
#[test]
fn a_quarter_round_moves_its_four_words_of_the_state_and_no_other() {
    let mut state: [u32; 16] = [
        0x8795_31e0, 0xc5ec_f37d, 0x5164_61b1, 0xc9a6_2f8a,
        0x44c2_0ef3, 0x3390_af7f, 0xd9fc_690b, 0x2a5f_714c,
        0x5337_2767, 0xb00a_5631, 0x974c_541a, 0x359e_9963,
        0x5c97_1061, 0x3d63_1689, 0x2098_d9d6, 0x91db_d320,
    ];
    chacha::quarter_round(&mut state, 2, 7, 8, 13);
    assert_eq!(
        state,
        [
            0x8795_31e0, 0xc5ec_f37d, 0xbdb8_86dc, 0xc9a6_2f8a,
            0x44c2_0ef3, 0x3390_af7f, 0xd9fc_690b, 0xcfac_afd2,
            0xe46b_ea80, 0xb00a_5631, 0x974c_541a, 0x359e_9963,
            0x5c97_1061, 0xccc0_7c79, 0x2098_d9d6, 0x91db_d320,
        ]
    );
}

/// RFC 8439 §2.3.2.
#[test]
fn the_block_function_is_rfc_8439s() {
    let key: Vec<u8> = (0u8..32).collect();
    let block = block_of(&key, 1, &hex("00 00 00 09 00 00 00 4a 00 00 00 00"));
    assert_eq!(
        block[..],
        hex("10 f1 e7 e4 d1 3b 59 15 50 0f dd 1f a3 20 71 c4
             c7 d1 f4 c7 33 c0 68 03 04 22 aa 9a c3 d4 6c 4e
             d2 82 64 46 07 9f aa 09 14 c2 d7 05 d9 8b 02 a2
             b5 12 9c d1 de 16 4e b9 cb d0 83 e8 a2 50 3c 4e")[..]
    );
}

/// RFC 8439 appendix A.1, its five vectors: a key, a block counter, a nonce
/// and the keystream block they give.
#[test]
fn the_block_function_gives_appendix_a_1s_keystreams() {
    let zeros = [0u8; 32];
    let mut last_one = [0u8; 32];
    last_one[31] = 1;
    let mut second_ff = [0u8; 32];
    second_ff[1] = 0xff;
    let zero_nonce = [0u8; 12];
    let mut nonce_two = [0u8; 12];
    nonce_two[11] = 2;
    let vectors: [(&[u8; 32], u32, &[u8; 12], &str); 5] = [
        (
            &zeros,
            0,
            &zero_nonce,
            "76 b8 e0 ad a0 f1 3d 90 40 5d 6a e5 53 86 bd 28
             bd d2 19 b8 a0 8d ed 1a a8 36 ef cc 8b 77 0d c7
             da 41 59 7c 51 57 48 8d 77 24 e0 3f b8 d8 4a 37
             6a 43 b8 f4 15 18 a1 1c c3 87 b6 69 b2 ee 65 86",
        ),
        (
            &zeros,
            1,
            &zero_nonce,
            "9f 07 e7 be 55 51 38 7a 98 ba 97 7c 73 2d 08 0d
             cb 0f 29 a0 48 e3 65 69 12 c6 53 3e 32 ee 7a ed
             29 b7 21 76 9c e6 4e 43 d5 71 33 b0 74 d8 39 d5
             31 ed 1f 28 51 0a fb 45 ac e1 0a 1f 4b 79 4d 6f",
        ),
        (
            &last_one,
            1,
            &zero_nonce,
            "3a eb 52 24 ec f8 49 92 9b 9d 82 8d b1 ce d4 dd
             83 20 25 e8 01 8b 81 60 b8 22 84 f3 c9 49 aa 5a
             8e ca 00 bb b4 a7 3b da d1 92 b5 c4 2f 73 f2 fd
             4e 27 36 44 c8 b3 61 25 a6 4a dd eb 00 6c 13 a0",
        ),
        (
            &second_ff,
            2,
            &zero_nonce,
            "72 d5 4d fb f1 2e c4 4b 36 26 92 df 94 13 7f 32
             8f ea 8d a7 39 90 26 5e c1 bb be a1 ae 9a f0 ca
             13 b2 5a a2 6c b4 a6 48 cb 9b 9d 1b e6 5b 2c 09
             24 a6 6c 54 d5 45 ec 1b 73 74 f4 87 2e 99 f0 96",
        ),
        (
            &zeros,
            0,
            &nonce_two,
            "c2 c6 4d 37 8c d5 36 37 4a e2 04 b9 ef 93 3f cd
             1a 8b 22 88 b3 df a4 96 72 ab 76 5b 54 ee 27 c7
             8a 97 0e 0e 95 5c 14 f3 a8 8e 74 1b 97 c2 86 f7
             5f 8f c2 99 e8 14 83 62 fa 19 8a 39 53 1b ed 6d",
        ),
    ];
    for (at, (key, counter, nonce, keystream)) in vectors.into_iter().enumerate() {
        assert_eq!(block_of(key, counter, nonce)[..], hex(keystream)[..], "test vector #{}", at + 1);
    }
}

/// Bytes [`Seed::judge`] takes: no two of its words alike.
fn seed_bytes(tag: u8) -> [u8; SEED_LEN] {
    core::array::from_fn(|at| tag ^ (at as u8).wrapping_mul(37))
}

fn seed(tag: u8) -> Seed {
    Seed::judge(&seed_bytes(tag)).expect("a seed")
}

fn drawn(generator: &mut Generator, len: usize) -> Vec<u8> {
    let mut out = std::vec![0u8; len];
    generator.stream().fill(&mut out);
    out
}

#[test]
fn bytes_a_failed_source_leaves_are_no_seed() {
    assert_eq!(Seed::judge(&[0u8; 32]).err(), Some(Refusal::Constant));
    assert_eq!(Seed::judge(&[0xffu8; 32]).err(), Some(Refusal::Constant));
    // A source stuck on one draw: four words, one value.
    let stuck: Vec<u8> = 0x0123_4567_89ab_cdefu64.to_ne_bytes().repeat(4);
    assert_eq!(Seed::judge(&stuck).err(), Some(Refusal::Constant));
    // One word of the four its own is a seed.
    let mut three_alike = [0u8; 32];
    three_alike[31] = 1;
    assert!(Seed::judge(&three_alike).is_ok());
    assert!(Seed::judge(&seed_bytes(0)).is_ok());
}

#[test]
fn bytes_of_another_length_are_no_seed() {
    let bytes = seed_bytes(0);
    assert_eq!(Seed::judge(&[]).err(), Some(Refusal::Length(0)));
    assert_eq!(Seed::judge(&bytes[..31]).err(), Some(Refusal::Length(31)));
    let mut long = bytes.to_vec();
    long.push(7);
    assert_eq!(Seed::judge(&long).err(), Some(Refusal::Length(33)));
}

/// The place a seed was handed in holds zeros once it is taken, on every arm:
/// accepted, refused for its bytes, refused for its length, and none handed.
#[test]
fn a_taken_seed_leaves_zeros_where_it_was_handed() {
    /// What is handed, its length, and the judgment the take owes it.
    type Arm = ([u8; SEED_LEN], u64, Option<Result<(), Refusal>>);
    let judged = |taken: &Option<Result<Seed, Refusal>>| taken.as_ref().map(|seed| seed.as_ref().map(|_| ()).map_err(|why| *why));
    let arms: [Arm; 5] = [
        (seed_bytes(4), 32, Some(Ok(()))),
        ([0xab; SEED_LEN], 32, Some(Err(Refusal::Constant))),
        (seed_bytes(5), 16, Some(Err(Refusal::Length(16)))),
        (seed_bytes(6), 33, Some(Err(Refusal::Length(33)))),
        (seed_bytes(7), 0, None),
    ];
    for (handed, len, want) in arms {
        let (mut bytes, mut at) = (handed, len);
        let taken = Seed::take(&mut bytes, &mut at);
        assert_eq!(judged(&taken), want, "{len} bytes handed");
        assert_eq!((bytes, at), ([0; SEED_LEN], 0), "{len} bytes handed");
        if let Some(Ok(seed)) = taken {
            assert_eq!(seed.0, handed, "the seed taken is the bytes handed");
        }
    }
}

/// The construction, block by block: the key is the mix block of the unkeyed
/// constant XOR the seed; a draw's block under it gives the next key and the
/// stream's; the stream is ChaCha20 under its own key from block 0.
#[test]
fn a_draw_is_the_blocks_the_module_states() {
    let bytes = seed_bytes(0x5a);
    let mixed: Vec<u8> = UNKEYED.iter().zip(bytes).map(|(k, s)| k ^ s).collect();
    let key = block_of(&mixed, 0, &MIX);
    let draw = block_of(&key[..32], 0, &DRAW);
    let (next_key, stream_key) = draw.split_at(32);
    let mut want = block_of(stream_key, 0, &[0; 12]).to_vec();
    want.extend_from_slice(&block_of(stream_key, 1, &[0; 12])[..36]);

    let mut generator = Generator::keyed(seed(0x5a));
    assert_eq!(drawn(&mut generator, 100), want);
    assert_eq!(generator.key[..], *next_key);
}

#[test]
fn the_key_a_draw_was_made_under_is_gone_when_it_returns() {
    let mut generator = Generator::keyed(seed(1));
    let before = generator.key;
    let first = drawn(&mut generator, 64);
    assert_ne!(generator.key, before);
    // The key left behind is not the stream's, and gives another draw.
    let second = drawn(&mut generator, 64);
    assert_ne!(first, second);
    let mut replayed = Generator { key: before };
    assert_eq!(drawn(&mut replayed, 64), first);
}

#[test]
fn every_seed_mixed_moves_the_draw_and_none_replaces_another() {
    let draw_of = |tags: &[u8]| {
        let mut generator = Generator::keyed(seed(tags[0]));
        for &tag in &tags[1..] {
            generator.mix(seed(tag));
        }
        drawn(&mut generator, 32)
    };
    let both = draw_of(&[1, 2]);
    assert_ne!(both, draw_of(&[1]));
    assert_ne!(both, draw_of(&[2]));
    // The later seed did not replace the earlier: the earlier still decides.
    assert_ne!(both, draw_of(&[3, 2]));
    assert_ne!(both, draw_of(&[1, 3]));
}

/// The block counter is 64 bits: the block after `u32::MAX` is counter 0 under
/// the next high half, not block 0 again.
#[test]
fn a_stream_does_not_repeat_where_32_bits_of_counter_end() {
    let mut generator = Generator::keyed(seed(9));
    let mut stream = generator.stream();
    let key = stream.key;
    stream.next = u64::from(u32::MAX);
    let mut out = [0u8; 128];
    stream.fill(&mut out);
    assert_eq!(out[..64], block_of(&key, u32::MAX, &[0; 12]));
    let mut high = [0u8; 12];
    high[0] = 1;
    assert_eq!(out[64..], block_of(&key, 0, &high));
    assert_ne!(out[64..], block_of(&key, 0, &[0; 12]));
}

#[test]
fn a_wipe_leaves_zeros() {
    let mut bytes = seed_bytes(3);
    wipe(&mut bytes);
    assert_eq!(bytes, [0; SEED_LEN]);
}
