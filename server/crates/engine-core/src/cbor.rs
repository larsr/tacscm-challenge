//! Minimal deterministic CBOR encoder for the architecture-gate value subset.

use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Self>),
    Map(Vec<(String, Self)>),
    Bool(bool),
    Null,
    /// Canonical CBOR produced by this encoder and cached as an immutable
    /// subtree. This keeps repeated state commitments linear in bytes hashed
    /// without repeatedly rebuilding large value trees.
    Encoded(Arc<[u8]>),
}

impl Value {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        self.encode_to(&mut |bytes| output.extend_from_slice(bytes));
        output
    }

    pub(crate) fn encode_to(&self, output: &mut impl FnMut(&[u8])) {
        match self {
            Self::U64(value) => encode_head(0, *value, output),
            Self::I64(value) if *value >= 0 => encode_head(0, *value as u64, output),
            Self::I64(value) => encode_head(
                1,
                u64::try_from(-1_i128 - i128::from(*value)).unwrap(),
                output,
            ),
            Self::Bytes(value) => {
                encode_head(2, u64::try_from(value.len()).unwrap(), output);
                output(value);
            }
            Self::Text(value) => encode_text(value, output),
            Self::Array(values) => {
                encode_head(4, u64::try_from(values.len()).unwrap(), output);
                for value in values {
                    value.encode_to(output);
                }
            }
            Self::Map(entries) => {
                let mut ordered: Vec<(Vec<u8>, &Self)> = entries
                    .iter()
                    .map(|(key, value)| {
                        let mut encoded_key = Vec::new();
                        encode_text(key, &mut |bytes| encoded_key.extend_from_slice(bytes));
                        (encoded_key, value)
                    })
                    .collect();
                ordered.sort_by(|(left, _), (right, _)| {
                    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
                });
                encode_head(5, u64::try_from(ordered.len()).unwrap(), output);
                for (key, value) in ordered {
                    output(&key);
                    value.encode_to(output);
                }
            }
            Self::Bool(false) => output(&[0xf4]),
            Self::Bool(true) => output(&[0xf5]),
            Self::Null => output(&[0xf6]),
            Self::Encoded(value) => output(value),
        }
    }
}

fn encode_text(value: &str, output: &mut impl FnMut(&[u8])) {
    encode_head(3, u64::try_from(value.len()).unwrap(), output);
    output(value.as_bytes());
}

fn encode_head(major: u8, argument: u64, output: &mut impl FnMut(&[u8])) {
    let prefix = major << 5;
    match argument {
        0..=23 => output(&[prefix | u8::try_from(argument).unwrap()]),
        24..=0xff => {
            output(&[prefix | 24, u8::try_from(argument).unwrap()]);
        }
        0x100..=0xffff => {
            output(&[prefix | 25]);
            output(&u16::try_from(argument).unwrap().to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            output(&[prefix | 26]);
            output(&u32::try_from(argument).unwrap().to_be_bytes());
        }
        _ => {
            output(&[prefix | 27]);
            output(&argument.to_be_bytes());
        }
    }
}

/// Decodes one value from the deterministic CBOR subset emitted by this crate.
///
/// The returned length permits callers to walk an RFC 8742 CBOR sequence
/// without accepting trailing bytes as part of the current item.
pub fn decode_one(input: &[u8]) -> Result<(Value, usize), &'static str> {
    fn argument(input: &[u8], position: &mut usize, additional: u8) -> Result<u64, &'static str> {
        let length = match additional {
            0..=23 => return Ok(u64::from(additional)),
            24 => 1,
            25 => 2,
            26 => 4,
            27 => 8,
            _ => return Err("indefinite or reserved CBOR argument"),
        };
        let end = position
            .checked_add(length)
            .ok_or("CBOR position overflow")?;
        let bytes = input.get(*position..end).ok_or("truncated CBOR argument")?;
        *position = end;
        let mut value = 0_u64;
        for byte in bytes {
            value = (value << 8) | u64::from(*byte);
        }
        Ok(value)
    }

    fn parse(input: &[u8], position: &mut usize) -> Result<Value, &'static str> {
        let initial = *input.get(*position).ok_or("truncated CBOR value")?;
        *position += 1;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        if major == 7 {
            return match additional {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                _ => Err("unsupported CBOR simple value"),
            };
        }
        let value = argument(input, position, additional)?;
        match major {
            0 => Ok(Value::U64(value)),
            1 => {
                let signed = -1_i128 - i128::from(value);
                Ok(Value::I64(
                    i64::try_from(signed).map_err(|_| "CBOR negative integer exceeds i64")?,
                ))
            }
            2 | 3 => {
                let length = usize::try_from(value).map_err(|_| "CBOR length exceeds usize")?;
                let end = position
                    .checked_add(length)
                    .ok_or("CBOR position overflow")?;
                let bytes = input.get(*position..end).ok_or("truncated CBOR string")?;
                *position = end;
                if major == 2 {
                    Ok(Value::Bytes(bytes.to_vec()))
                } else {
                    Ok(Value::Text(
                        std::str::from_utf8(bytes)
                            .map_err(|_| "invalid CBOR UTF-8")?
                            .to_owned(),
                    ))
                }
            }
            4 => {
                let length = usize::try_from(value).map_err(|_| "CBOR length exceeds usize")?;
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(parse(input, position)?);
                }
                Ok(Value::Array(values))
            }
            5 => {
                let length = usize::try_from(value).map_err(|_| "CBOR length exceeds usize")?;
                let mut fields = Vec::with_capacity(length);
                for _ in 0..length {
                    let Value::Text(key) = parse(input, position)? else {
                        return Err("non-text CBOR map key");
                    };
                    fields.push((key, parse(input, position)?));
                }
                Ok(Value::Map(fields))
            }
            _ => Err("unsupported CBOR major type"),
        }
    }

    let mut position = 0;
    let value = parse(input, &mut position)?;
    Ok((value, position))
}

#[cfg(test)]
mod tests {
    use super::{Value, decode_one};
    use crate::crypto::hex;

    #[test]
    fn map_keys_use_deterministic_length_first_order() {
        let value = Value::Map(vec![
            ("bbb".into(), Value::U64(2)),
            ("a".into(), Value::U64(1)),
            ("aa".into(), Value::U64(3)),
        ]);
        assert_eq!(hex(&value.encode()), "a3616101626161036362626202");
        let encoded = value.encode();
        let (decoded, consumed) = decode_one(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.encode(), encoded);
    }
}
