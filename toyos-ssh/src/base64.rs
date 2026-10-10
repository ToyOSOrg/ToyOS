//! RFC 4648 base64, the armour of an OpenSSH key file and the digits of a
//! fingerprint.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The bytes `text` spells, its whitespace dropped: padded and canonical,
/// what [`encode`] writes and nothing else, so one blob has one spelling.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, &'static str> {
    let digits: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let pad = digits.iter().rev().take_while(|&&c| c == b'=').count();
    if !digits.len().is_multiple_of(4) || pad > 2 {
        return Err("base64 that is not whole padded groups of four");
    }
    let mut out = Vec::with_capacity(digits.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in &digits[..digits.len() - pad] {
        acc = acc << 6 | ALPHABET.iter().position(|a| a == c).ok_or("a character that is not base64")? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if acc != 0 {
        return Err("base64 with bits set past its last byte");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 §10's vectors, both ways, and every other spelling refused.
    #[test]
    fn rfc_4648_vectors() {
        for (plain, armour) in
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(encode(plain.as_bytes()), armour);
            assert_eq!(decode(armour).as_deref(), Ok(plain.as_bytes()));
        }
        assert_eq!(decode("Zm9v\nYmFy\n").as_deref(), Ok(&b"foobar"[..]));
        for bent in ["Zm9vY", "Zg=", "Zm9vYg", "Z==="] {
            assert_eq!(decode(bent), Err("base64 that is not whole padded groups of four"), "{bent:?}");
        }
        for bent in ["Zm9!", "Zg=a", "Z=g=", "Zg==Zg=="] {
            assert_eq!(decode(bent), Err("a character that is not base64"), "{bent:?}");
        }
        for bent in ["Zh==", "Zm9="] {
            assert_eq!(decode(bent), Err("base64 with bits set past its last byte"), "{bent:?}");
        }
    }
}
