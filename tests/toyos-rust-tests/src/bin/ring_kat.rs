//! `ring`'s primitives against the known answers their specifications
//! publish: the oracle under every TLS connection a ToyOS program makes, whose
//! own handshake only tests `ring` against `ring` on the host. Each answer is
//! the cited document's, not one this program or `ring` computed.

#![allow(deprecated, reason = "`ring::test::rand` is the one way to give an X25519 key known bytes")]

use ring::{aead, agreement, digest, hmac, rand, signature};

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "odd hex {s:?}");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn check(what: &str, got: &[u8], want: &str) {
    assert_eq!(got, hex(want).as_slice(), "{what}");
    println!("ring_kat: {what} ok");
}

fn digests() {
    // FIPS 180-2, appendix B.1 and C.1, and B.3.
    check("sha256(abc)", digest::digest(&digest::SHA256, b"abc").as_ref(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    check("sha512(abc)", digest::digest(&digest::SHA512, b"abc").as_ref(),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
         2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f");
    check("sha256(a * 1000000)", digest::digest(&digest::SHA256, &vec![b'a'; 1_000_000]).as_ref(),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
}

fn hmac_sha256() {
    // RFC 4231, test case 2.
    let key = hmac::Key::new(hmac::HMAC_SHA256, b"Jefe");
    check("hmac-sha256 rfc4231 case 2", hmac::sign(&key, b"what do ya want for nothing?").as_ref(),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
}

fn x25519() {
    // RFC 7748, section 6.1.
    let alice = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let bob_public = hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    let rng = ring::test::rand::FixedSliceRandom { bytes: &alice };
    let private = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).expect("a key of alice's bytes");
    check("x25519 rfc7748 alice's public key", private.compute_public_key().expect("alice's public key").as_ref(),
        "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, bob_public);
    let shared = agreement::agree_ephemeral(private, &peer, |k| k.to_vec()).expect("the agreement");
    check("x25519 rfc7748 shared secret", &shared,
        "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
}

/// Seal `plaintext` and check the ciphertext and tag, then open it, and
/// refuse it with one bit of the tag flipped.
fn aead_case(what: &str, alg: &'static aead::Algorithm, key: &str, nonce: &str, aad: &str, plaintext: &[u8], ciphertext: &str, tag: &str) {
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(alg, &hex(key)).expect("the key"));
    let nonce = || aead::Nonce::try_assume_unique_for_key(&hex(nonce)).expect("the nonce");
    let aad = hex(aad);
    let mut sealed = plaintext.to_vec();
    key.seal_in_place_append_tag(nonce(), aead::Aad::from(&aad), &mut sealed).expect("seal");
    let (body, got_tag) = sealed.split_at(plaintext.len());
    check(&format!("{what} ciphertext"), body, ciphertext);
    check(&format!("{what} tag"), got_tag, tag);
    let mut opened = sealed.clone();
    let plain = key.open_in_place(nonce(), aead::Aad::from(&aad), &mut opened).expect("open");
    assert_eq!(plain, plaintext, "{what} opens to its plaintext");
    let mut forged = sealed;
    *forged.last_mut().expect("a tag") ^= 1;
    assert!(key.open_in_place(nonce(), aead::Aad::from(&aad), &mut forged).is_err(), "{what} opens a forged tag");
    println!("ring_kat: {what} opened, and refused with a flipped tag bit");
}

fn aeads() {
    // RFC 8439, section 2.8.2.
    aead_case(
        "chacha20-poly1305 rfc8439 2.8.2",
        &aead::CHACHA20_POLY1305,
        "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
        "070000004041424344454647",
        "50515253c0c1c2c3c4c5c6c7",
        b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.",
        "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
         3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
         92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
         3ff4def08e4b7a9de576d26586cec64b6116",
        "1ae10b594f09e26a7e902ecbd0600691",
    );
    // McGrew and Viega, "The Galois/Counter Mode of Operation", test cases 4
    // and 16.
    let plaintext = hex(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
         1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
    );
    aead_case(
        "aes-128-gcm case 4",
        &aead::AES_128_GCM,
        "feffe9928665731c6d6a8f9467308308",
        "cafebabefacedbaddecaf888",
        "feedfacedeadbeeffeedfacedeadbeefabaddad2",
        &plaintext,
        "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e\
         21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
        "5bc94fbc3221a5db94fae95ae7121a47",
    );
    aead_case(
        "aes-256-gcm case 16",
        &aead::AES_256_GCM,
        "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308",
        "cafebabefacedbaddecaf888",
        "feedfacedeadbeeffeedfacedeadbeefabaddad2",
        &plaintext,
        "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
         8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
        "76fc6ece0f4e1768cddf8853bb2d551b",
    );
}

fn signatures() {
    // RFC 8032, section 7.1, test 1: the key pair from its seed, the
    // signature of the empty message, and a refusal of another message.
    let pair = signature::Ed25519KeyPair::from_seed_unchecked(&hex(
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
    ))
    .expect("the seed");
    use signature::KeyPair;
    check("ed25519 rfc8032 test 1 public key", pair.public_key().as_ref(),
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let sig = pair.sign(b"");
    check("ed25519 rfc8032 test 1 signature", sig.as_ref(),
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b");
    let public = signature::UnparsedPublicKey::new(&signature::ED25519, pair.public_key().as_ref().to_vec());
    assert!(public.verify(b"x", sig.as_ref()).is_err(), "ed25519 verifies another message");

    // RFC 6979, appendix A.2.5: P-256, SHA-256, the message "sample".
    let public = signature::UnparsedPublicKey::new(
        &signature::ECDSA_P256_SHA256_FIXED,
        hex("0460fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6\
             7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299"),
    );
    let sig = hex(
        "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716\
         f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8",
    );
    public.verify(b"sample", &sig).expect("ecdsa p-256 rfc6979 a.2.5 verifies");
    assert!(public.verify(b"samplf", &sig).is_err(), "ecdsa p-256 verifies another message");
    println!("ring_kat: ecdsa p-256 rfc6979 a.2.5 verified, and refused for another message");
}

fn system_random() {
    use rand::SecureRandom;
    let rng = rand::SystemRandom::new();
    let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
    rng.fill(&mut a).expect("the system's random bytes");
    rng.fill(&mut b).expect("the system's random bytes");
    assert_ne!(a, b, "two fills of SystemRandom are the same 32 bytes");
    println!("ring_kat: SystemRandom filled two different 32-byte buffers");
}

fn main() {
    digests();
    hmac_sha256();
    x25519();
    aeads();
    signatures();
    system_random();
    println!("ring_kat: ok");
}
