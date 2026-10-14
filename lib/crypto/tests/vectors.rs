use toyos_crypto::{
    Error,
    aead::{OpeningKey, SealingKey},
    hash::{Sha256, Sha384, Sha512},
    kdf::{Hkdf, Hmac},
};

fn hex<const N: usize>(text: &str) -> [u8; N] {
    assert_eq!(text.len(), 2 * N);
    core::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

#[test]
fn fips_sha2_abc_and_empty() {
    assert_eq!(
        Sha256::digest(b"abc").unwrap(),
        hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(
        Sha384::digest(b"abc").unwrap(),
        hex(
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        )
    );
    assert_eq!(
        Sha512::digest(b"abc").unwrap(),
        hex(
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        )
    );
    assert_eq!(
        Sha256::digest(b"").unwrap(),
        hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
}

#[test]
fn rfc4231_hmac_sha256() {
    let mut out = [0; 32];
    Hmac::<Sha256>::authenticate(&[0x0b; 20], b"Hi There", &mut out).unwrap();
    assert_eq!(
        out,
        hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
    );
}

#[test]
fn rfc5869_case_one_and_output_bound() {
    let salt = hex::<13>("000102030405060708090a0b0c");
    let info = hex::<10>("f0f1f2f3f4f5f6f7f8f9");
    let key = Hkdf::<Sha256>::extract(&salt, &[0x0b; 22]).unwrap();
    let mut out = [0; 42];
    key.expand(&[&info], &mut out).unwrap();
    assert_eq!(
        out,
        hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865")
    );
    assert_eq!(key.expand(&[], &mut [0; 8161]), Err(Error::Length));
}

#[test]
fn rfc8439_aead_and_authenticated_refusal() {
    let key = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce = hex("070000004041424344454647");
    let aad = hex::<12>("50515253c0c1c2c3c4c5c6c7");
    let message=b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let mut encrypted = message.to_vec();
    let mut sealer = SealingKey::chacha20_poly1305(key, nonce);
    let (used, tag) = sealer.seal(&aad, &mut encrypted).unwrap();
    assert_eq!(used, nonce);
    assert_eq!(tag, hex("1ae10b594f09e26a7e902ecbd0600691"));
    assert_eq!(
        encrypted,
        hex::<114>(
            "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116"
        )
    );
    let opener = OpeningKey::chacha20_poly1305(key);
    let original = encrypted.clone();
    let mut bad = tag;
    bad[0] ^= 1;
    assert_eq!(
        opener.open(&nonce, &aad, &mut encrypted, &bad),
        Err(Error::Authentication)
    );
    assert_eq!(encrypted, original);
    assert_eq!(
        opener.open(&nonce, &aad, &mut encrypted, &tag[..15]),
        Err(Error::Length)
    );
    opener.open(&nonce, &aad, &mut encrypted, &tag).unwrap();
    assert_eq!(encrypted, message);
}

#[test]
fn rfc7748_x25519() {
    use toyos_crypto::x25519::{PublicKey, SecretKey};
    let a = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let b = hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let ap = SecretKey::from_bytes(a).public_key();
    let bp = SecretKey::from_bytes(b).public_key();
    assert_eq!(
        ap.0,
        hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
    );
    assert_eq!(
        bp.0,
        hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
    );
    assert_eq!(
        *SecretKey::from_bytes(a).agree(&bp).unwrap().as_bytes(),
        hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")
    );
    assert!(SecretKey::from_bytes(a).agree(&PublicKey([0; 32])).is_err());
}

#[test]
fn rfc8032_empty_and_strict_rejection() {
    use toyos_crypto::ed25519::PublicKey;
    let public = PublicKey::from_bytes(hex(
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
    ))
    .unwrap();
    let signature = hex::<64>(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    );
    public.verify(b"", &signature).unwrap();
    assert!(public.verify(b"x", &signature).is_err());
    let mut high = signature;
    high[63] |= 0x80;
    assert!(public.verify(b"", &high).is_err());
    let order = hex::<32>("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
    let mut malleable = signature;
    let mut carry = 0u16;
    for i in 0..32 {
        let v = u16::from(malleable[32 + i]) + u16::from(order[i]) + carry;
        malleable[32 + i] = v as u8;
        carry = v >> 8;
    }
    assert!(public.verify(b"", &malleable).is_err());
    assert!(public.verify(b"", &signature[..63]).is_err());
    let mut identity = [0; 32];
    identity[0] = 1;
    assert!(PublicKey::from_bytes(identity).is_err());
    let mut bad_r = signature;
    bad_r[..32].copy_from_slice(&identity);
    assert!(public.verify(b"", &bad_r).is_err());
    #[cfg(feature = "sign")]
    {
        let signer = toyos_crypto::ed25519::SigningKey::from_seed(hex(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        ));
        assert_eq!(signer.public_key().as_bytes(), public.as_bytes());
        assert_eq!(signer.sign(b"").unwrap(), signature);
    }
}

#[test]
fn malformed_public_boundaries() {
    use toyos_crypto::ecc::{p256, p384};
    for n in 0..70 {
        let b = vec![0; n];
        assert!(p256::PublicKey::from_sec1(&b).is_err());
    }
    let mut point = [0; 65];
    point[0] = 4;
    point[32] = 1;
    point[64] = 1;
    assert!(p256::PublicKey::from_sec1(&point).is_err());
    let mut point = [0; 97];
    point[0] = 4;
    point[48] = 1;
    point[96] = 1;
    assert!(p384::PublicKey::from_sec1(&point).is_err());
    assert!(p256::SecretKey::from_bytes([0; 32]).is_err());
    assert!(p256::SecretKey::from_bytes([255; 32]).is_err());
    assert!(p384::SecretKey::from_bytes([0; 48]).is_err());
    for n in [0, 1, 255, 256, 513] {
        assert!(toyos_crypto::rsa::PublicKey::from_components(&vec![0; n], &[1, 0, 1]).is_err());
    }
    let mut pk = [0; 1184];
    pk[0] = 1;
    pk[1] = 13;
    assert!(toyos_crypto::mlkem::EncapsulationKey::from_bytes(&pk).is_err());
    assert!(toyos_crypto::mlkem::EncapsulationKey::from_bytes(&pk[..1183]).is_err());
}

#[test]
fn nist_gcm_zero_key_zero_block() {
    let mut data = [0; 16];
    let mut key = SealingKey::aes128_gcm([0; 16], [0; 12]);
    let (_, tag) = key.seal(&[], &mut data).unwrap();
    assert_eq!(data, hex("0388dace60b6a392f328c2b971b2fe78"));
    assert_eq!(tag, hex("ab6e47d42cec13bdf53a67b21257bddf"));
}
