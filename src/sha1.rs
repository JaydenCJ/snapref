//! Dependency-free SHA-1 (FIPS 180-1) used for content addressing.
//!
//! snapref uses SHA-1 exactly the way git does: as a content-addressing
//! function inside a local, trusted object store — not as a security
//! boundary against adversarial collisions. Blob ids are computed over
//! `blob <len>\0<bytes>`, which makes them byte-for-byte identical to git
//! blob ids, so any snapref blob can be cross-checked with
//! `git hash-object <file>`.

/// Compute the SHA-1 digest of `data` as a lowercase hex string.
pub fn hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(40);
    for byte in digest(data) {
        let hi = byte >> 4;
        let lo = byte & 0xf;
        out.push(char::from_digit(hi as u32, 16).unwrap());
        out.push(char::from_digit(lo as u32, 16).unwrap());
    }
    out
}

/// Compute the raw 20-byte SHA-1 digest of `data`.
pub fn digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    // Message padding: 0x80, zeros to 56 mod 64, then the bit length (big endian).
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_matches_reference_vector() {
        assert_eq!(hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn abc_matches_reference_vector() {
        assert_eq!(hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn two_block_nist_vector() {
        // Crosses the 64-byte block boundary, exercising multi-chunk processing.
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn million_a_nist_vector() {
        // The classic long-message vector; catches length-encoding bugs.
        let data = vec![b'a'; 1_000_000];
        assert_eq!(hex(&data), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn git_empty_blob_id_is_reproduced() {
        // `git hash-object` of an empty file — snapref blob ids must agree.
        assert_eq!(hex(b"blob 0\0"), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn git_hello_world_blob_id_is_reproduced() {
        // `echo "hello world" | git hash-object --stdin`
        assert_eq!(
            hex(b"blob 12\0hello world\n"),
            "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
        );
    }
}
