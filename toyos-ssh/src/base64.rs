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

/// The bytes `text` spells, whitespace and `=` skipped.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, &'static str> {
    let digits: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in digits.chunks(4) {
        if chunk.len() == 1 {
            return Err("base64 that ends one digit into a group");
        }
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = ALPHABET.iter().position(|a| a == c).ok_or("a character that is not base64")?;
            n |= (v as u32) << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 §10's vectors, both ways.
    #[test]
    fn rfc_4648_vectors() {
        for (plain, armour) in
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(encode(plain.as_bytes()), armour);
            assert_eq!(decode(armour).as_deref(), Ok(plain.as_bytes()));
        }
        assert_eq!(decode("Zm9vY"), Err("base64 that ends one digit into a group"));
        assert_eq!(decode("Zm9!"), Err("a character that is not base64"));
    }
}
