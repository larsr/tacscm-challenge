//! Implementations of the contract's cryptographic primitives.

use sha2::{Digest, Sha256};

use crate::cbor::Value;

#[must_use]
pub fn sha256(input: &[u8]) -> [u8; 32] {
    Sha256::digest(input).into()
}

/// Hashes canonical CBOR without materializing one large intermediate buffer.
#[must_use]
pub(crate) fn sha256_value(value: &Value) -> [u8; 32] {
    let mut hasher = Sha256::new();
    value.encode_to(&mut |bytes| hasher.update(bytes));
    hasher.finalize().into()
}

#[must_use]
pub fn hmac_sha256(key: &[u8], input: &[u8]) -> [u8; 32] {
    let mut key_block = [0_u8; 64];
    if key.len() > 64 {
        key_block[..32].copy_from_slice(&sha256(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + input.len());
    let mut outer = Vec::with_capacity(96);
    for byte in key_block {
        inner.push(byte ^ 0x36);
        outer.push(byte ^ 0x5c);
    }
    inner.extend_from_slice(input);
    outer.extend_from_slice(&sha256(&inner));
    sha256(&outer)
}

#[must_use]
pub fn derive_stream_key(root_seed: &[u8; 32], profile: &str, stream: &str) -> [u8; 32] {
    let mut input = Vec::with_capacity(profile.len() + stream.len() + 1);
    input.extend_from_slice(profile.as_bytes());
    input.push(0);
    input.extend_from_slice(stream.as_bytes());
    hmac_sha256(root_seed, &input)
}

fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    let (mut x_a, mut x_b, mut x_c, mut x_d) = (state[a], state[b], state[c], state[d]);
    x_a = x_a.wrapping_add(x_b);
    x_d ^= x_a;
    x_d = x_d.rotate_left(16);
    x_c = x_c.wrapping_add(x_d);
    x_b ^= x_c;
    x_b = x_b.rotate_left(12);
    x_a = x_a.wrapping_add(x_b);
    x_d ^= x_a;
    x_d = x_d.rotate_left(8);
    x_c = x_c.wrapping_add(x_d);
    x_b ^= x_c;
    x_b = x_b.rotate_left(7);
    state[a] = x_a;
    state[b] = x_b;
    state[c] = x_c;
    state[d] = x_d;
}

#[must_use]
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut initial = [0_u32; 16];
    initial[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for (slot, bytes) in initial[4..12].iter_mut().zip(key.chunks_exact(4)) {
        *slot = u32::from_le_bytes(bytes.try_into().expect("four-byte word"));
    }
    initial[12] = counter;
    for (slot, bytes) in initial[13..16].iter_mut().zip(nonce.chunks_exact(4)) {
        *slot = u32::from_le_bytes(bytes.try_into().expect("four-byte word"));
    }

    let mut working = initial;
    for _ in 0..10 {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 8, 13);
        quarter_round(&mut working, 3, 4, 9, 14);
    }

    let mut output = [0_u8; 64];
    for (chunk, (value, base)) in output
        .chunks_exact_mut(4)
        .zip(working.into_iter().zip(initial))
    {
        chunk.copy_from_slice(&value.wrapping_add(base).to_le_bytes());
    }
    output
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{chacha20_block, derive_stream_key, hex, hmac_sha256, sha256};

    #[test]
    fn standard_hash_vectors() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&hmac_sha256(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn contract_named_stream_vector() {
        let mut seed = [0_u8; 32];
        for (index, byte) in seed.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("seed index fits u8");
        }
        let key = derive_stream_key(&seed, "tac-scm-2026-draft1", "config/loan-rate");
        assert_eq!(
            hex(&key),
            "bc4f868d0cbc43eb17ccfc357f0c57930cdd75671e4613bee0f09227884671aa"
        );
        let block = chacha20_block(&key, 0, &[0_u8; 12]);
        assert_eq!(
            hex(&block),
            "6ba33f0824656e15c07e74c54ace3f9463a4295c3f6a5303939570a44cf619891678a0b133a3814a84e341fc688deaa7788710325c4d083a1561736719965cc4"
        );
        assert_eq!(
            u64::from_le_bytes(block[..8].try_into().unwrap()),
            1_544_282_927_666_275_179
        );
    }
}
