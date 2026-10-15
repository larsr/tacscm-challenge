//! Exact integer arithmetic used by the bounded economic transition.

use core::fmt;

const RATE_SCALE: u128 = 1_000_000_000;
const GAME_DAYS: u128 = 220;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArithmeticError {
    DivisionByZero,
    Overflow,
}

impl fmt::Display for ArithmeticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DivisionByZero => formatter.write_str("division by zero"),
            Self::Overflow => formatter.write_str("exact arithmetic overflow"),
        }
    }
}

impl std::error::Error for ArithmeticError {}

/// Rounds a non-negative rational to the nearest integer, ties to even.
pub fn round_half_even(numerator: u128, denominator: u128) -> Result<u128, ArithmeticError> {
    if denominator == 0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let doubled = remainder.checked_mul(2).ok_or(ArithmeticError::Overflow)?;
    if doubled > denominator || (doubled == denominator && quotient % 2 == 1) {
        quotient.checked_add(1).ok_or(ArithmeticError::Overflow)
    } else {
        Ok(quotient)
    }
}

/// Returns the positive daily debit for a negative opening cash balance.
pub fn daily_loan_interest(
    opening_cash: i64,
    annual_rate_ppb: u32,
) -> Result<i64, ArithmeticError> {
    if opening_cash >= 0 {
        return Ok(0);
    }
    let magnitude =
        u128::try_from(-i128::from(opening_cash)).map_err(|_| ArithmeticError::Overflow)?;
    let numerator = magnitude
        .checked_mul(u128::from(annual_rate_ppb))
        .ok_or(ArithmeticError::Overflow)?;
    let amount = round_half_even(numerator, RATE_SCALE * GAME_DAYS)?;
    i64::try_from(amount).map_err(|_| ArithmeticError::Overflow)
}

/// Applies a non-negative rate to a non-negative amount using profile rounding.
pub fn mul_rate(amount: u128, rate_ppb: u32) -> Result<u128, ArithmeticError> {
    round_half_even(
        amount
            .checked_mul(u128::from(rate_ppb))
            .ok_or(ArithmeticError::Overflow)?,
        RATE_SCALE,
    )
}

/// Returns one simulated-day interest amount for either loan or deposit cash.
pub fn daily_interest(balance: i64, annual_rate_ppb: u32) -> Result<i64, ArithmeticError> {
    let magnitude = if balance < 0 {
        u128::try_from(-i128::from(balance)).map_err(|_| ArithmeticError::Overflow)?
    } else {
        u128::try_from(balance).map_err(|_| ArithmeticError::Overflow)?
    };
    let numerator = magnitude
        .checked_mul(u128::from(annual_rate_ppb))
        .ok_or(ArithmeticError::Overflow)?;
    i64::try_from(round_half_even(numerator, RATE_SCALE * GAME_DAYS)?)
        .map_err(|_| ArithmeticError::Overflow)
}

/// Profile storage charge for the aggregate base value held at inventory close.
pub fn storage_charge(
    inventory_value_microcredits: u128,
    annual_rate_ppb: u32,
) -> Result<i64, ArithmeticError> {
    let numerator = inventory_value_microcredits
        .checked_mul(u128::from(annual_rate_ppb))
        .ok_or(ArithmeticError::Overflow)?;
    i64::try_from(round_half_even(numerator, RATE_SCALE * GAME_DAYS)?)
        .map_err(|_| ArithmeticError::Overflow)
}

/// Posted supplier price from a pre-batch supply and commitment snapshot.
pub fn supplier_posted_price(
    base_microcredits: i64,
    supply: u64,
    committed: u64,
) -> Result<i64, ArithmeticError> {
    let base = u128::try_from(base_microcredits).map_err(|_| ArithmeticError::Overflow)?;
    if supply == 0 {
        return i64::try_from(base.checked_mul(3).ok_or(ArithmeticError::Overflow)? / 2)
            .map_err(|_| ArithmeticError::Overflow);
    }
    // base * (1/2 + min(2, committed/supply)/2)
    let bounded = committed.min(supply.saturating_mul(2));
    let numerator = base
        .checked_mul(u128::from(supply.saturating_add(bounded)))
        .ok_or(ArithmeticError::Overflow)?;
    i64::try_from(round_half_even(numerator, u128::from(supply) * 2)?)
        .map_err(|_| ArithmeticError::Overflow)
}

/// Exact supplier capacity transition, truncating the final rational toward zero.
pub fn supplier_capacity(prior: u32, raw: u64) -> Result<u32, ArithmeticError> {
    let two64 = i128::from(u64::MAX) + 1;
    let denominator = 200_i128
        .checked_mul(two64)
        .ok_or(ArithmeticError::Overflow)?;
    let numerator = i128::from(prior)
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(11_000_i128 * i128::from(raw)))
        .and_then(|value| value.checked_sub(5_500_i128 * two64))
        .and_then(|value| value.checked_add((550_i128 - i128::from(prior)) * 2 * two64))
        .ok_or(ArithmeticError::Overflow)?;
    u32::try_from((numerator / denominator).max(1)).map_err(|_| ArithmeticError::Overflow)
}

/// Customer trend transition using nano-unit Q and ppb trend.
pub fn customer_trend(
    q_nano: u64,
    trend_ppb: u64,
    epsilon_ppb: i64,
    q_min_nano: u64,
    q_max_nano: u64,
) -> Result<(u64, u64), ArithmeticError> {
    let candidate_trend =
        (i128::from(trend_ppb) + i128::from(epsilon_ppb)).clamp(950_000_000, 1_052_631_579);
    let candidate_q = round_half_even(
        u128::from(q_nano)
            .checked_mul(u128::try_from(candidate_trend).map_err(|_| ArithmeticError::Overflow)?)
            .ok_or(ArithmeticError::Overflow)?,
        RATE_SCALE,
    )?;
    let candidate_q = u64::try_from(candidate_q).map_err(|_| ArithmeticError::Overflow)?;
    if candidate_q < q_min_nano {
        Ok((q_min_nano, 1_000_000_000))
    } else if candidate_q > q_max_nano {
        Ok((q_max_nano, 1_000_000_000))
    } else {
        Ok((
            candidate_q,
            u64::try_from(candidate_trend).map_err(|_| ArithmeticError::Overflow)?,
        ))
    }
}

/// Down payment and final partial-delivery settlement.
pub fn supplier_final_settlement(
    ordered_quantity: u32,
    delivered_quantity: u32,
    unit_price_microcredits: i64,
    original_down_microcredits: i64,
) -> Result<(i64, i64, i64), ArithmeticError> {
    if ordered_quantity == 0 || delivered_quantity > ordered_quantity {
        return Err(ArithmeticError::DivisionByZero);
    }
    let retained = round_half_even(
        u128::try_from(original_down_microcredits)
            .map_err(|_| ArithmeticError::Overflow)?
            .checked_mul(u128::from(delivered_quantity))
            .ok_or(ArithmeticError::Overflow)?,
        u128::from(ordered_quantity),
    )?;
    let retained = i64::try_from(retained).map_err(|_| ArithmeticError::Overflow)?;
    let refund = original_down_microcredits
        .checked_sub(retained)
        .ok_or(ArithmeticError::Overflow)?;
    let delivered_total = i128::from(delivered_quantity)
        .checked_mul(i128::from(unit_price_microcredits))
        .and_then(|value| value.checked_sub(i128::from(retained)))
        .ok_or(ArithmeticError::Overflow)?;
    Ok((
        retained,
        refund,
        i64::try_from(delivered_total).map_err(|_| ArithmeticError::Overflow)?,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocationRequest {
    pub id: String,
    pub quantity: u32,
    pub reputation_ppm: u32,
    pub due_day: u32,
    /// Uniform rank sampled by the engine for otherwise exact economic ties.
    pub symmetry_rank: u64,
}

/// Weighted progressive filling for one cumulative capacity budget.
pub fn weighted_allocation(mut budget: u32, requests: &[AllocationRequest]) -> Vec<(String, u32)> {
    let mut allocated = vec![0_u32; requests.len()];
    while budget > 0 {
        let candidate = requests
            .iter()
            .enumerate()
            .filter(|(index, request)| allocated[*index] < request.quantity)
            .min_by(|(left_index, left), (right_index, right)| {
                let left_rep = u128::from(left.reputation_ppm.max(1));
                let right_rep = u128::from(right.reputation_ppm.max(1));
                let left_denominator =
                    u128::from(left.quantity).saturating_mul(left_rep.saturating_pow(3));
                let right_denominator =
                    u128::from(right.quantity).saturating_mul(right_rep.saturating_pow(3));
                let left_cross =
                    u128::from(allocated[*left_index] + 1).saturating_mul(right_denominator);
                let right_cross =
                    u128::from(allocated[*right_index] + 1).saturating_mul(left_denominator);
                left_cross.cmp(&right_cross).then_with(|| {
                    (left.due_day, left.symmetry_rank).cmp(&(right.due_day, right.symmetry_rank))
                })
            });
        let Some((index, _)) = candidate else { break };
        allocated[index] += 1;
        budget -= 1;
    }
    requests
        .iter()
        .zip(allocated)
        .map(|(request, quantity)| (request.id.clone(), quantity))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        AllocationRequest, customer_trend, daily_loan_interest, round_half_even, storage_charge,
        supplier_capacity, supplier_final_settlement, supplier_posted_price, weighted_allocation,
    };

    #[test]
    fn ties_round_to_even() {
        assert_eq!(round_half_even(5, 2), Ok(2));
        assert_eq!(round_half_even(7, 2), Ok(4));
        assert_eq!(round_half_even(1, 2), Ok(0));
        assert_eq!(round_half_even(3, 2), Ok(2));
    }

    #[test]
    fn matches_contract_interest_vector() {
        assert_eq!(
            daily_loan_interest(-1_000_000_000, 100_000_000),
            Ok(454_545)
        );
        assert_eq!(daily_loan_interest(0, 100_000_000), Ok(0));
        assert_eq!(daily_loan_interest(1_000_000_000, 100_000_000), Ok(0));
    }

    #[test]
    fn remaining_scalar_contract_vectors() {
        assert_eq!(storage_charge(3_950_000_000, 250_000_000), Ok(4_488_636));
        assert_eq!(supplier_posted_price(100_000_000, 100, 0), Ok(50_000_000));
        assert_eq!(supplier_posted_price(100_000_000, 100, 60), Ok(80_000_000));
        assert_eq!(supplier_capacity(550, 0), Ok(522));
        assert_eq!(supplier_capacity(700, 1_u64 << 63), Ok(698));
        assert_eq!(
            supplier_final_settlement(10, 4, 100, 100),
            Ok((40, 60, 360))
        );
        assert_eq!(
            customer_trend(
                25_000_000_000,
                950_000_000,
                -10_000_000,
                25_000_000_000,
                100_000_000_000,
            ),
            Ok((25_000_000_000, 1_000_000_000))
        );
    }

    #[test]
    fn weighted_supplier_vectors() {
        let equal = [
            AllocationRequest {
                id: "a".into(),
                quantity: 60,
                reputation_ppm: 1_000_000,
                due_day: 5,
                symmetry_rank: 0,
            },
            AllocationRequest {
                id: "b".into(),
                quantity: 60,
                reputation_ppm: 1_000_000,
                due_day: 5,
                symmetry_rank: 1,
            },
        ];
        assert_eq!(
            weighted_allocation(100, &equal),
            vec![("a".into(), 50), ("b".into(), 50)]
        );
        let reputation = [
            AllocationRequest {
                id: "a".into(),
                quantity: 10,
                reputation_ppm: 1_000_000,
                due_day: 5,
                symmetry_rank: 0,
            },
            AllocationRequest {
                id: "b".into(),
                quantity: 10,
                reputation_ppm: 500_000,
                due_day: 5,
                symmetry_rank: 1,
            },
        ];
        assert_eq!(
            weighted_allocation(10, &reputation),
            vec![("a".into(), 9), ("b".into(), 1)]
        );

        let mut first = equal.clone();
        first[0].quantity = 1;
        first[1].quantity = 1;
        assert_eq!(
            weighted_allocation(1, &first),
            vec![("a".into(), 1), ("b".into(), 0)]
        );
        first[0].symmetry_rank = 1;
        first[1].symmetry_rank = 0;
        assert_eq!(
            weighted_allocation(1, &first),
            vec![("a".into(), 0), ("b".into(), 1)]
        );
    }
}
