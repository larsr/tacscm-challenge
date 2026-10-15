//! Deterministic named random streams and exact integer samplers.

use std::collections::BTreeMap;

use crate::cbor::Value;
use crate::crypto::{chacha20_block, derive_stream_key, sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RandomError {
    DuplicateCandidate,
    EmptyRange,
    RangeOverflow,
    StreamExhausted,
    PoissonDidNotConverge,
}

/// A single authoritative draw, suitable for canonical event recording.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RandomDraw {
    pub stream: String,
    pub ordinal: u64,
    pub raw: u64,
    pub derived: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UniformSelection {
    pub candidates: Vec<String>,
    pub selected_index: usize,
    pub winner: String,
    pub rejected_draws: Vec<RandomDraw>,
    pub accepted_draw: RandomDraw,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UniformPermutation {
    pub order: Vec<String>,
    pub selections: Vec<UniformSelection>,
}

/// Named ChaCha20 streams. Each stream begins at offset zero and is independent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedStreams {
    root_seed: [u8; 32],
    profile_id: String,
    positions: BTreeMap<String, u64>,
    retired_commitment: [u8; 32],
    retired_count: u64,
}

impl NamedStreams {
    #[must_use]
    pub fn new(root_seed: [u8; 32], profile_id: &str) -> Self {
        Self {
            root_seed,
            profile_id: profile_id.into(),
            positions: BTreeMap::new(),
            retired_commitment: [0; 32],
            retired_count: 0,
        }
    }

    pub fn draw_u64(&mut self, stream: &str) -> Result<RandomDraw, RandomError> {
        let ordinal = *self.positions.get(stream).unwrap_or(&0);
        let byte_position = ordinal.checked_mul(8).ok_or(RandomError::StreamExhausted)?;
        let block_index = byte_position / 64;
        let counter = u32::try_from(block_index).map_err(|_| RandomError::StreamExhausted)?;
        let offset = usize::try_from(byte_position % 64).expect("block offset fits usize");
        let key = derive_stream_key(&self.root_seed, &self.profile_id, stream);
        let block = chacha20_block(&key, counter, &[0_u8; 12]);
        let raw = u64::from_le_bytes(
            block[offset..offset + 8]
                .try_into()
                .expect("aligned draw fits block"),
        );
        self.positions.insert(
            stream.into(),
            ordinal.checked_add(1).ok_or(RandomError::StreamExhausted)?,
        );
        Ok(RandomDraw {
            stream: stream.into(),
            ordinal,
            raw,
            derived: i128::from(raw),
        })
    }

    pub fn uniform_u64(
        &mut self,
        stream: &str,
        minimum: u64,
        maximum: u64,
    ) -> Result<(u64, Vec<RandomDraw>), RandomError> {
        let width = maximum
            .checked_sub(minimum)
            .and_then(|delta| delta.checked_add(1))
            .ok_or(RandomError::EmptyRange)?;
        let space = u128::from(u64::MAX) + 1;
        let limit = (space / u128::from(width)) * u128::from(width);
        let mut draws = Vec::new();
        loop {
            let mut draw = self.draw_u64(stream)?;
            if u128::from(draw.raw) < limit {
                let result = minimum
                    .checked_add(draw.raw % width)
                    .ok_or(RandomError::RangeOverflow)?;
                draw.derived = i128::from(result);
                draws.push(draw);
                self.retire(stream)?;
                return Ok((result, draws));
            }
            draw.derived = -1;
            draws.push(draw);
        }
    }

    pub fn uniform_i64(
        &mut self,
        stream: &str,
        minimum: i64,
        maximum: i64,
    ) -> Result<(i64, Vec<RandomDraw>), RandomError> {
        let width = i128::from(maximum) - i128::from(minimum) + 1;
        let width = u64::try_from(width).map_err(|_| RandomError::EmptyRange)?;
        let (offset, mut draws) = self.uniform_u64(stream, 0, width - 1)?;
        let result = i64::try_from(i128::from(minimum) + i128::from(offset))
            .map_err(|_| RandomError::RangeOverflow)?;
        if let Some(draw) = draws.last_mut() {
            draw.derived = i128::from(result);
        }
        Ok((result, draws))
    }

    pub fn uniform_permutation(
        &mut self,
        stream_prefix: &str,
        mut candidates: Vec<String>,
    ) -> Result<UniformPermutation, RandomError> {
        candidates.sort();
        if candidates.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RandomError::DuplicateCandidate);
        }
        let mut remaining = candidates;
        let mut order = Vec::with_capacity(remaining.len());
        let mut selections = Vec::with_capacity(remaining.len().saturating_sub(1));
        let draw_count = remaining.len().saturating_sub(1);
        for rank in 0..draw_count {
            let stream = format!("{stream_prefix}/rank/{rank}");
            let (selected_index, draws) =
                self.uniform_u64(&stream, 0, u64::try_from(remaining.len() - 1).unwrap())?;
            let selected_index = usize::try_from(selected_index).unwrap();
            let (accepted_draw, rejected_draws) = draws
                .split_last()
                .expect("uniform selection has one accepted draw");
            let winner = remaining[selected_index].clone();
            selections.push(UniformSelection {
                candidates: remaining.clone(),
                selected_index,
                winner: winner.clone(),
                rejected_draws: rejected_draws.to_vec(),
                accepted_draw: accepted_draw.clone(),
            });
            order.push(winner);
            remaining.remove(selected_index);
        }
        order.extend(remaining);
        Ok(UniformPermutation { order, selections })
    }

    /// Samples a Poisson value by exact rational comparison against a directed
    /// Taylor interval for exp(lambda). No floating-point value determines state.
    pub fn poisson(
        &mut self,
        stream: &str,
        mean_nano: u64,
    ) -> Result<(u32, RandomDraw), RandomError> {
        let mut draw = self.draw_u64(stream)?;
        let value = poisson_from_u64(mean_nano, draw.raw)?;
        draw.derived = i128::from(value);
        self.retire(stream)?;
        Ok((value, draw))
    }

    /// Draws the sole value from an addressed one-shot stream and retires it.
    pub fn draw_once_u64(&mut self, stream: &str) -> Result<RandomDraw, RandomError> {
        let draw = self.draw_u64(stream)?;
        self.retire(stream)?;
        Ok(draw)
    }

    fn retire(&mut self, stream: &str) -> Result<(), RandomError> {
        let ordinal = self
            .positions
            .remove(stream)
            .ok_or(RandomError::StreamExhausted)?;
        let entry = Value::Array(vec![
            Value::Text(stream.into()),
            Value::U64(ordinal.checked_mul(8).ok_or(RandomError::StreamExhausted)?),
        ])
        .encode();
        let mut material = Vec::with_capacity(32 + entry.len());
        material.extend_from_slice(&self.retired_commitment);
        material.extend_from_slice(&entry);
        self.retired_commitment = sha256(&material);
        self.retired_count = self
            .retired_count
            .checked_add(1)
            .ok_or(RandomError::StreamExhausted)?;
        Ok(())
    }

    #[must_use]
    pub fn byte_positions(&self) -> Vec<(String, u64)> {
        self.positions
            .iter()
            .map(|(name, ordinal)| (name.clone(), ordinal.saturating_mul(8)))
            .collect()
    }

    #[must_use]
    pub fn retired_state(&self) -> ([u8; 32], u64) {
        (self.retired_commitment, self.retired_count)
    }
}

/// A directed non-negative Q448 interval endpoint. The extra 128 fractional
/// bits are the mandatory precision escalation above the original Q320
/// implementation; two guard limbs remain above one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fixed([u32; 16]);

impl Ord for Fixed {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for Fixed {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Fixed {
    const ZERO: Self = Self([0; 16]);

    fn one() -> Self {
        let mut limbs = [0; 16];
        limbs[14] = 1;
        Self(limbs)
    }

    fn add(self, other: Self) -> Self {
        let mut output = [0; 16];
        let mut carry = 0_u64;
        for (index, slot) in output.iter_mut().enumerate() {
            let sum = u64::from(self.0[index]) + u64::from(other.0[index]) + carry;
            *slot = sum as u32;
            carry = sum >> 32;
        }
        debug_assert_eq!(carry, 0);
        Self(output)
    }

    fn sub(self, other: Self) -> Self {
        let mut output = [0; 16];
        let mut borrow = 0_i64;
        for (index, slot) in output.iter_mut().enumerate() {
            let difference = i64::from(self.0[index]) - i64::from(other.0[index]) - borrow;
            if difference < 0 {
                *slot = (difference + (1_i64 << 32)) as u32;
                borrow = 1;
            } else {
                *slot = difference as u32;
                borrow = 0;
            }
        }
        debug_assert_eq!(borrow, 0);
        Self(output)
    }

    fn mul_div_u64(self, factor: u64, divisor: u64, round_up: bool) -> Self {
        let mut expanded = [0_u32; 18];
        let mut carry = 0_u128;
        for (index, limb) in self.0.iter().enumerate() {
            let product = u128::from(*limb) * u128::from(factor) + carry;
            expanded[index] = product as u32;
            carry = product >> 32;
        }
        for slot in &mut expanded[16..] {
            *slot = carry as u32;
            carry >>= 32;
        }
        let mut remainder = 0_u128;
        for limb in expanded.iter_mut().rev() {
            let dividend = (remainder << 32) | u128::from(*limb);
            *limb = u32::try_from(dividend / u128::from(divisor)).expect("quotient limb");
            remainder = dividend % u128::from(divisor);
        }
        debug_assert!(expanded[16..].iter().all(|limb| *limb == 0));
        let mut output = [0; 16];
        output.copy_from_slice(&expanded[..16]);
        let mut result = Self(output);
        if round_up && remainder != 0 {
            result = result.add(Self([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        }
        result
    }

    fn square(self, round_up: bool) -> Self {
        let mut product = [0_u64; 32];
        for left in 0..16 {
            for right in 0..16 {
                product[left + right] = product[left + right]
                    .checked_add(u64::from(self.0[left]) * u64::from(self.0[right]))
                    .expect("guarded fixed-point product");
                let mut index = left + right;
                while product[index] > u64::from(u32::MAX) {
                    let carry = product[index] >> 32;
                    product[index] &= u64::from(u32::MAX);
                    index += 1;
                    product[index] = product[index].checked_add(carry).expect("product carry");
                }
            }
        }
        for index in 0..31 {
            let carry = product[index] >> 32;
            product[index] &= u64::from(u32::MAX);
            product[index + 1] += carry;
        }
        let discarded = product[..14].iter().any(|limb| *limb != 0);
        let mut output = [0; 16];
        for (slot, limb) in output.iter_mut().zip(&product[14..30]) {
            *slot = u32::try_from(*limb).expect("normalized product limb");
        }
        let mut result = Self(output);
        if round_up && discarded {
            result = result.add(Self([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        }
        result
    }

    fn compare_draw(self, raw: u64) -> std::cmp::Ordering {
        let mut draw = [0_u32; 16];
        draw[12] = raw as u32;
        draw[13] = (raw >> 32) as u32;
        self.cmp(&Self(draw))
    }
}

fn exp_negative_interval(mean_nano: u64) -> (Fixed, Fixed) {
    const SCALE_TIMES_256: u64 = 256_000_000_000;
    let mut term_lower = Fixed::one();
    let mut term_upper = Fixed::one();
    let mut lower = Fixed::one();
    let mut upper = Fixed::one();
    for index in 1..=192_u64 {
        let divisor = SCALE_TIMES_256 * index;
        term_lower = term_lower.mul_div_u64(mean_nano, divisor, false);
        term_upper = term_upper.mul_div_u64(mean_nano, divisor, true);
        if index % 2 == 1 {
            lower = lower.sub(term_upper);
            upper = upper.sub(term_lower);
        } else {
            lower = lower.add(term_lower);
            upper = upper.add(term_upper);
        }
        if term_upper == Fixed::ZERO {
            break;
        }
    }
    // Alternating Taylor partial sums already enclose exp(-lambda/256).
    for _ in 0..8 {
        lower = lower.square(false);
        upper = upper.square(true);
    }
    (lower, upper)
}

/// Exact inverse-CDF sampler for nano-unit means used by the profile. All
/// comparisons use directed Q448 intervals, never binary floating point.
pub fn poisson_from_u64(mean_nano: u64, raw: u64) -> Result<u32, RandomError> {
    if mean_nano == 0 || raw == 0 {
        return Ok(0);
    }
    if mean_nano > 120_000_000_000 {
        return Err(RandomError::PoissonDidNotConverge);
    }
    let (mut probability_lower, mut probability_upper) = exp_negative_interval(mean_nano);
    let mut cdf_lower = probability_lower;
    let mut cdf_upper = probability_upper;
    for value in 0..=512_u32 {
        if cdf_lower.compare_draw(raw).is_gt() {
            return Ok(value);
        }
        if cdf_upper.compare_draw(raw).is_gt() {
            return Err(RandomError::PoissonDidNotConverge);
        }
        let divisor = 1_000_000_000_u64 * u64::from(value + 1);
        probability_lower = probability_lower.mul_div_u64(mean_nano, divisor, false);
        probability_upper = probability_upper.mul_div_u64(mean_nano, divisor, true);
        cdf_lower = cdf_lower.add(probability_lower);
        cdf_upper = cdf_upper.add(probability_upper);
    }
    Err(RandomError::PoissonDidNotConverge)
}

#[cfg(test)]
mod tests {
    use super::{NamedStreams, poisson_from_u64};

    #[test]
    fn scalar_poisson_boundaries_are_exact() {
        assert_eq!(poisson_from_u64(25_000_000_000, 0), Ok(0));
        assert_eq!(
            poisson_from_u64(25_000_000_000, 9_223_372_036_854_775_808),
            Ok(25)
        );
        assert_eq!(
            poisson_from_u64(25_000_000_000, 8_732_660_394_345_015_011),
            Ok(24)
        );
        assert_eq!(
            poisson_from_u64(25_000_000_000, 8_732_660_394_345_015_012),
            Ok(25)
        );
        assert_eq!(
            poisson_from_u64(100_000_000_000, 9_223_372_036_854_775_808),
            Ok(100)
        );
    }

    #[test]
    fn escalated_poisson_precision_resolves_profile_domain_guards() {
        let draws = [1, u64::MAX / 4, u64::MAX / 2, u64::MAX - 1, u64::MAX];
        for whole_mean in 25..=120_u64 {
            for raw in draws {
                assert!(
                    poisson_from_u64(whole_mean * 1_000_000_000, raw).is_ok(),
                    "mean={whole_mean}, raw={raw}",
                );
            }
        }
    }

    #[test]
    fn named_stream_uniform_vector_matches_contract() {
        let mut seed = [0_u8; 32];
        for (index, byte) in seed.iter_mut().enumerate() {
            *byte = u8::try_from(index).unwrap();
        }
        let mut streams = NamedStreams::new(seed, "tac-scm-2026-draft1");
        let (value, draws) = streams.uniform_u64("config/loan-rate", 600, 1200).unwrap();
        assert_eq!(value, 999);
        assert_eq!(draws.len(), 1);
        assert_eq!(draws[0].raw, 1_544_282_927_666_275_179);
        assert!(streams.byte_positions().is_empty());
        assert_eq!(streams.retired_state().1, 1);
    }
}
