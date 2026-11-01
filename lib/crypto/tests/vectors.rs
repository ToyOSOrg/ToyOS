use toyos_crypto::{
    Error,
    aead::{OpeningKey, SealingKey},
    hash::{Sha256, Sha384, Sha512},
    kdf::{Hkdf, Hmac, pbkdf2},
    scrypt,
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
    assert_eq!(encrypted, [0; 114], "a refusal zeroes the record");
    encrypted = original;
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

/// The RFC's hex, its whitespace dropped.
fn spaced<const N: usize>(text: &str) -> [u8; N] {
    hex(&text.split_whitespace().collect::<String>())
}

/// RFC 7914 §11.
#[test]
fn rfc7914_pbkdf2_hmac_sha256() {
    let mut out = [0; 64];
    pbkdf2::<Sha256>(b"passwd", b"salt", 1.try_into().unwrap(), &mut out).unwrap();
    assert_eq!(
        out,
        spaced(
            "55 ac 04 6e 56 e3 08 9f ec 16 91 c2 25 44 b6 05 f9 41 85 21 6d de 04 65 e6 8b 9d 57 c2 0d ac bc
             49 ca 9c cc f1 79 b6 45 99 16 64 b3 9d 77 ef 31 7c 71 b8 45 b1 e3 0b d5 09 11 20 41 d3 a1 97 83"
        )
    );
    pbkdf2::<Sha256>(b"Password", b"NaCl", 80_000.try_into().unwrap(), &mut out).unwrap();
    assert_eq!(
        out,
        spaced(
            "4d dc d8 f6 0b 98 be 21 83 0c ee 5e f2 27 01 f9 64 1a 44 18 d0 4c 04 14 ae ff 08 87 6b 34 ab 56
             a1 d4 25 a1 22 58 33 54 9a db 84 1b 51 c9 b3 17 6a 27 2b de bb a1 d0 78 47 8f 62 b3 97 f3 3c 8d"
        )
    );
    assert_eq!(pbkdf2::<Sha256>(b"", b"", 1.try_into().unwrap(), &mut []), Err(Error::Length));
}

/// RFC 7914 §12, all four: the last fills `scrypt::MAX_MEMORY`.
#[test]
fn rfc7914_scrypt() {
    /// Passphrase, salt, log2 N, r, p and the derived key.
    type Case = (&'static [u8], &'static [u8], u8, u32, u32, &'static str);
    let cases: [Case; 4] = [
        (b"", b"", 4, 1, 1, "77 d6 57 62 38 65 7b 20 3b 19 ca 42 c1 8a 04 97 f1 6b 48 44 e3 07 4a e8 df df fa 3f ed e2 14 42
                           fc d0 06 9d ed 09 48 f8 32 6a 75 3a 0f c8 1f 17 e8 d3 e0 fb 2e 0d 36 28 cf 35 e2 0c 38 d1 89 06"),
        (b"password", b"NaCl", 10, 8, 16, "fd ba be 1c 9d 34 72 00 78 56 e7 19 0d 01 e9 fe 7c 6a d7 cb c8 23 78 30 e7 73 76 63 4b 37 31 62
                           2e af 30 d9 2e 22 a3 88 6f f1 09 27 9d 98 30 da c7 27 af b9 4a 83 ee 6d 83 60 cb df a2 cc 06 40"),
        (b"pleaseletmein", b"SodiumChloride", 14, 8, 1, "70 23 bd cb 3a fd 73 48 46 1c 06 cd 81 fd 38 eb fd a8 fb ba 90 4f 8e 3e a9 b5 43 f6 54 5d a1 f2
                           d5 43 29 55 61 3f 0f cf 62 d4 97 05 24 2a 9a f9 e6 1e 85 dc 0d 65 1e 40 df cf 01 7b 45 57 58 87"),
        (b"pleaseletmein", b"SodiumChloride", 20, 8, 1, "21 01 cb 9b 6a 51 1a ae ad db be 09 cf 70 f8 81 ec 56 8d 57 4a 2f fd 4d ab e5 ee 98 20 ad aa 47
                           8e 56 fd 8f 4b a5 d0 9f fa 1c 6d 92 7c 40 f4 c3 37 30 40 49 e8 a9 52 fb cb f4 5c 6f a7 7a 41 a4"),
    ];
    for (passphrase, salt, log_n, r, p, expected) in cases {
        let params = scrypt::Params::new(log_n, r, p, scrypt::Limits { memory: usize::MAX, work: u64::MAX }).unwrap();
        let mut workspace = vec![0; params.workspace_len()];
        let mut out = [0; 64];
        scrypt::derive(passphrase, salt, params, &mut workspace, &mut out).unwrap();
        assert_eq!(out, spaced::<64>(expected), "N = 2^{log_n}, r = {r}, p = {p}");
    }
}

/// IEEE Std 802.11-2020 Annex J: the PRF's four test vectors and the pass-phrase-to-PSK
/// mapping's two beyond `differential.rs`'s ("password", "IEEE").
#[cfg(feature = "wpa2")]
#[test]
fn ieee80211_annex_j_prf_and_psk() {
    use toyos_crypto::wpa2;
    fn prf<const N: usize>(key: &[u8], label: &str, data: &str) -> [u8; N] {
        let mut out = [0; N];
        wpa2::prf(key, label.as_bytes(), data.as_bytes(), &mut out).unwrap();
        out
    }
    assert_eq!(
        prf::<24>(&[0x0b; 20], "prefix", "Hi There"),
        hex("bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606")
    );
    assert_eq!(
        prf::<32>(b"Jefe", "prefix-2", "what do ya want for nothing?"),
        hex("47c4908e30c947521ad20be9053450ecbea23d3aa604b77326d8b3825ff7475c")
    );
    assert_eq!(
        prf::<48>(&[0xaa; 80], "prefix-3", "Test Using Larger Than Block-Size Key - Hash Key First"),
        hex("0ab6c33ccf70d0d736f4b04c8a7373255511abc5073713163bd0b8c9eeb7e1956fa066820a73ddee3f6d3bd407e0682a")
    );
    assert_eq!(
        prf::<64>(&[0x0b; 20], "prefix-4", "Hi There Again"),
        hex(
            "248cfbc532ab38ffa483c8a2e40bf170eb542a2e0916d7bf6d97da2c4c5ca877\
             736c53a65b03fa4b3745ce7613f6ad68e0e4a798b7cf691c96176fd634a59a49"
        )
    );
    assert_eq!(
        wpa2::derive_pmk(b"ThisIsAPassword", b"ThisIsASSID").unwrap().as_bytes(),
        &hex("0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af")
    );
    assert_eq!(
        wpa2::derive_pmk(&[b'a'; 32], &[b'Z'; 32]).unwrap().as_bytes(),
        &hex("becb93866bb8c3832cb777c2f559807c8c59afcb6eae734885001300a981cc62")
    );
}

/// RFC 6070's one PBKDF2-HMAC-SHA1 case at the PMK's 4096 iterations whose password and salt
/// a pass-phrase and SSID can be: its 20 octets are the PMK's first.
#[cfg(feature = "wpa2")]
#[test]
fn rfc6070_pbkdf2_hmac_sha1_at_4096_iterations() {
    let pmk = toyos_crypto::wpa2::derive_pmk(b"password", b"salt").unwrap();
    assert_eq!(pmk.as_bytes()[..20], hex::<20>("4b007901b765489abead49d926f721d065a429c1"));
}
