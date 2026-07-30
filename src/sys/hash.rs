/*! FNV-1a, inline instead of std's DefaultHasher for anything that touches
disk. DefaultHasher is documented as unstable across Rust releases, so keys
built from it silently rotate on toolchain bumps, poison for cache
directories and install stamps that have to survive upgrades. */

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

#[cfg(test)]
mod tests {
    use super::*;

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
