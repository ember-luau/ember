/*! FNV-1a, inline instead of std's DefaultHasher for anything that touches
disk. DefaultHasher is documented as unstable across Rust releases, so keys
built from it silently rotate on toolchain bumps, poison for cache
directories and install stamps that have to survive upgrades. FNV is no
defence against a changed file, so package archives get SHA-256. */

const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a of `bytes`.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/** one hash over several parts, kept unambiguous by hashing each part's
length first, so ("ab", "c") never collides with ("a", "bc"). */
pub fn fnv1a_parts(parts: &[&[u8]]) -> u64 {
    let mut hash = OFFSET_BASIS;
    for part in parts {
        for byte in (part.len() as u64).to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
        for byte in *part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/** lowercase hex SHA-256 of `bytes`, the `sha256` of an index entry and of
ember.lock. it is what `sha256sum` prints. */
pub fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/** whether `text` has the shape of [`sha256_hex`]: 64 lowercase hex
characters. */
pub fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_sha256_hex(&sha256_hex(b"abc")));
        assert!(!is_sha256_hex(&sha256_hex(b"abc").to_uppercase()));
        assert!(!is_sha256_hex("abc"));
    }

    #[test]
    fn matches_the_published_fnv1a_vectors() {
        // from the reference implementation's test suite
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn part_boundaries_are_part_of_the_hash() {
        assert_ne!(fnv1a_parts(&[b"ab", b"c"]), fnv1a_parts(&[b"a", b"bc"]));
        assert_ne!(fnv1a_parts(&[b"ab"]), fnv1a_parts(&[b"ab", b""]));
        assert_eq!(fnv1a_parts(&[b"ab", b"c"]), fnv1a_parts(&[b"ab", b"c"]));
    }
}
