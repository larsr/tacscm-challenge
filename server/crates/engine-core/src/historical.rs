//! Independently specified research-only historical-rule counterfactuals.
//!
//! These functions implement an independently written research contract for
//! historical-rule counterfactuals (`planned-research`). The challenge profile
//! never uses them. They are not translations of a historical server and cannot
//! establish equivalence with any historical implementation.

use core::fmt;

use crate::arithmetic::{ArithmeticError, round_half_even};

pub const RESEARCH_PROFILE_REVISION: u32 = 1;
pub const MICROCREDITS_PER_CREDIT: i64 = 1_000_000;
const RATE_PERCENT_PPB: u32 = 10_000_000;
const DECIMAL_SCALE: u128 = 1_000_000_000_000_000_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoricalFactors {
    pub financial_carry: bool,
    pub terminal_settlement: bool,
    pub ordering_fairness: bool,
    pub supplier_market: bool,
}

impl HistoricalFactors {
    pub const NONE: Self = Self {
        financial_carry: false,
        terminal_settlement: false,
        ordering_fairness: false,
        supplier_market: false,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoricalProfile {
    FinancialCarry,
    TerminalSettlement,
    OrderCoupled,
    SupplierMarket,
    OrderSupplier,
    SupplierTerminal,
    FinancialTerminal,
}

impl HistoricalProfile {
    pub const ALL: [Self; 7] = [
        Self::FinancialCarry,
        Self::TerminalSettlement,
        Self::OrderCoupled,
        Self::SupplierMarket,
        Self::OrderSupplier,
        Self::SupplierTerminal,
        Self::FinancialTerminal,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::FinancialCarry => "tac-scm-research-hf-financial-carry-v1",
            Self::TerminalSettlement => "tac-scm-research-hf-terminal-settlement-v1",
            Self::OrderCoupled => "tac-scm-research-hf-order-coupled-v1",
            Self::SupplierMarket => "tac-scm-research-hf-supplier-market-v1",
            Self::OrderSupplier => "tac-scm-research-hf-order-supplier-v1",
            Self::SupplierTerminal => "tac-scm-research-hf-supplier-terminal-v1",
            Self::FinancialTerminal => "tac-scm-research-hf-financial-terminal-v1",
        }
    }

    #[must_use]
    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|profile| profile.id() == value)
    }

    #[must_use]
    pub const fn factors(self) -> HistoricalFactors {
        match self {
            Self::FinancialCarry => HistoricalFactors {
                financial_carry: true,
                ..HistoricalFactors::NONE
            },
            Self::TerminalSettlement => HistoricalFactors {
                terminal_settlement: true,
                ..HistoricalFactors::NONE
            },
            Self::OrderCoupled => HistoricalFactors {
                ordering_fairness: true,
                ..HistoricalFactors::NONE
            },
            Self::SupplierMarket => HistoricalFactors {
                supplier_market: true,
                ..HistoricalFactors::NONE
            },
            Self::OrderSupplier => HistoricalFactors {
                ordering_fairness: true,
                supplier_market: true,
                ..HistoricalFactors::NONE
            },
            Self::SupplierTerminal => HistoricalFactors {
                terminal_settlement: true,
                supplier_market: true,
                ..HistoricalFactors::NONE
            },
            Self::FinancialTerminal => HistoricalFactors {
                financial_carry: true,
                terminal_settlement: true,
                ..HistoricalFactors::NONE
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoricalError {
    Arithmetic(ArithmeticError),
    InvalidRate,
    InvalidSample,
    NonWholeCreditInventory,
    PrecisionAmbiguous,
}

impl fmt::Display for HistoricalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arithmetic(error) => write!(formatter, "{error}"),
            Self::InvalidRate => formatter.write_str("historical rate is outside its contract"),
            Self::InvalidSample => {
                formatter.write_str("historical categorical sample is outside its contract")
            }
            Self::NonWholeCreditInventory => {
                formatter.write_str("historical inventory value is not a whole credit")
            }
            Self::PrecisionAmbiguous => formatter
                .write_str("24-decimal directed storage bounds do not prove one rounded credit"),
        }
    }
}

impl std::error::Error for HistoricalError {}

impl From<ArithmeticError> for HistoricalError {
    fn from(value: ArithmeticError) -> Self {
        Self::Arithmetic(value)
    }
}

pub fn historical_loan_percent(sample: u64) -> Result<u32, HistoricalError> {
    if sample >= 12 {
        return Err(HistoricalError::InvalidSample);
    }
    Ok(if sample == 0 {
        6
    } else if sample == 11 {
        12
    } else {
        6 + u32::try_from(sample.div_ceil(2)).expect("bounded sample fits u32")
    })
}

pub fn historical_storage_percent(sample: u64) -> Result<u32, HistoricalError> {
    if sample >= 50 {
        return Err(HistoricalError::InvalidSample);
    }
    Ok(if sample == 0 {
        25
    } else if sample == 49 {
        50
    } else {
        25 + u32::try_from(sample.div_ceil(2)).expect("bounded sample fits u32")
    })
}

#[must_use]
pub const fn percent_to_ppb(percent: u32) -> u32 {
    percent * RATE_PERCENT_PPB
}

fn floor_div(numerator: i128, denominator: i128) -> i128 {
    debug_assert!(denominator > 0);
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder < 0 {
        quotient - 1
    } else {
        quotient
    }
}

/// Historical whole-credit balance after one daily simple-interest update.
pub fn historical_finance_balance(
    opening_microcredits: i64,
    annual_percent: u32,
) -> Result<i64, HistoricalError> {
    if annual_percent > 100 {
        return Err(HistoricalError::InvalidRate);
    }
    let denominator = i128::from(MICROCREDITS_PER_CREDIT) * 22_000;
    let numerator = i128::from(opening_microcredits)
        .checked_mul(i128::from(22_000 + annual_percent))
        .ok_or(ArithmeticError::Overflow)?;
    let rounded_credits = floor_div(
        numerator
            .checked_mul(2)
            .and_then(|value| value.checked_add(denominator))
            .ok_or(ArithmeticError::Overflow)?,
        denominator * 2,
    );
    let balance = rounded_credits
        .checked_mul(i128::from(MICROCREDITS_PER_CREDIT))
        .ok_or(ArithmeticError::Overflow)?;
    i64::try_from(balance).map_err(|_| HistoricalError::Arithmetic(ArithmeticError::Overflow))
}

const STORAGE_DAILY_RATE_FLOOR_24: [u128; 26] = [
    1_014_803_434_523_333_978_234,
    1_051_059_799_171_806_485_902,
    1_087_030_846_905_632_214_427,
    1_122_721_043_261_559_370_028,
    1_158_134_749_676_545_354_189,
    1_193_276_226_699_678_137_707,
    1_228_149_637_081_141_551_502,
    1_262_759_048_743_830_698_765,
    1_297_108_437_642_927_691_850,
    1_331_201_690_518_469_644_999,
    1_365_042_607_545_679_124_802,
    1_398_634_904_887_580_975_858,
    1_431_982_217_154_197_581_566,
    1_465_088_099_772_396_245_485,
    1_497_956_031_270_256_611_349,
    1_530_589_415_479_632_066_843,
    1_562_991_583_660_396_142_195,
    1_595_165_796_549_692_317_939,
    1_627_115_246_339_342_744_097,
    1_658_843_058_584_417_538_496,
    1_690_352_294_045_821_009_774,
    1_721_645_950_469_613_814_598,
    1_752_726_964_305_660_218_252,
    1_783_598_212_368_066_825_965,
    1_814_262_513_439_762_962_719,
    1_844_722_629_823_462_903_910,
];

fn round_half_up(numerator: u128, denominator: u128) -> Result<u128, ArithmeticError> {
    if denominator == 0 {
        return Err(ArithmeticError::DivisionByZero);
    }
    numerator
        .checked_mul(2)
        .and_then(|value| value.checked_add(denominator))
        .map(|value| value / (denominator * 2))
        .ok_or(ArithmeticError::Overflow)
}

/// Historical storage charge with directed decimal bounds around the root.
pub fn historical_storage_charge(
    inventory_value_microcredits: u128,
    annual_percent: u32,
) -> Result<i64, HistoricalError> {
    let unit = u128::try_from(MICROCREDITS_PER_CREDIT).expect("positive unit fits u128");
    if !inventory_value_microcredits.is_multiple_of(unit) {
        return Err(HistoricalError::NonWholeCreditInventory);
    }
    let index = usize::try_from(
        annual_percent
            .checked_sub(25)
            .filter(|value| *value <= 25)
            .ok_or(HistoricalError::InvalidRate)?,
    )
    .expect("bounded rate fits usize");
    let inventory_credits = inventory_value_microcredits / unit;
    let lower_rate = STORAGE_DAILY_RATE_FLOOR_24[index];
    let lower = round_half_up(
        inventory_credits
            .checked_mul(lower_rate)
            .ok_or(ArithmeticError::Overflow)?,
        DECIMAL_SCALE,
    )?;
    let upper = round_half_up(
        inventory_credits
            .checked_mul(lower_rate + 1)
            .ok_or(ArithmeticError::Overflow)?,
        DECIMAL_SCALE,
    )?;
    if lower != upper {
        return Err(HistoricalError::PrecisionAmbiguous);
    }
    let charge = lower.checked_mul(unit).ok_or(ArithmeticError::Overflow)?;
    i64::try_from(charge).map_err(|_| HistoricalError::Arithmetic(ArithmeticError::Overflow))
}

/// Historical final supplier payment: original down payment is never refunded.
pub fn historical_supplier_final_settlement(
    delivered_quantity: u32,
    unit_price_microcredits: i64,
    original_down_microcredits: i64,
) -> Result<(i64, i64, i64), HistoricalError> {
    if unit_price_microcredits < 0 || original_down_microcredits < 0 {
        return Err(HistoricalError::Arithmetic(ArithmeticError::Overflow));
    }
    let delivered_total = i128::from(delivered_quantity)
        .checked_mul(i128::from(unit_price_microcredits))
        .and_then(|value| value.checked_sub(i128::from(original_down_microcredits)))
        .ok_or(ArithmeticError::Overflow)?;
    let debit = i64::try_from(delivered_total.max(0))
        .map_err(|_| HistoricalError::Arithmetic(ArithmeticError::Overflow))?;
    Ok((original_down_microcredits, 0, debit))
}

/// Total deterministic supplier observable-approximation price.
#[allow(clippy::too_many_arguments)]
pub fn historical_supplier_price(
    base_microcredits: i64,
    cumulative_supply: u64,
    committed_quantity: u64,
    batch_requested_quantity: u64,
    probe_count: u64,
    actual_capacity: u32,
    reputation_ppm: u32,
) -> Result<i64, HistoricalError> {
    if base_microcredits < 0 || reputation_ppm > 1_000_000 {
        return Err(HistoricalError::InvalidRate);
    }
    let probe_units = u64::from((actual_capacity / 20).max(1))
        .checked_mul(probe_count)
        .ok_or(ArithmeticError::Overflow)?;
    let pressure_quantity = committed_quantity
        .checked_add(batch_requested_quantity)
        .and_then(|value| value.checked_add(probe_units))
        .ok_or(ArithmeticError::Overflow)?;
    let pressure_ppm = if cumulative_supply == 0 {
        2_000_000_u128
    } else {
        round_half_even(
            u128::from(pressure_quantity)
                .checked_mul(1_000_000)
                .ok_or(ArithmeticError::Overflow)?,
            u128::from(cumulative_supply),
        )?
        .min(2_000_000)
    };
    let multiplier_ppm = 500_000_u128
        .checked_add(pressure_ppm / 2)
        .and_then(|value| value.checked_add(u128::from(1_000_000 - reputation_ppm) / 4))
        .ok_or(ArithmeticError::Overflow)?;
    let price = round_half_even(
        u128::try_from(base_microcredits)
            .map_err(|_| ArithmeticError::Overflow)?
            .checked_mul(multiplier_ppm)
            .ok_or(ArithmeticError::Overflow)?,
        1_000_000,
    )?;
    i64::try_from(price).map_err(|_| HistoricalError::Arithmetic(ArithmeticError::Overflow))
}

#[cfg(test)]
mod tests {
    use super::{
        HistoricalProfile, historical_finance_balance, historical_loan_percent,
        historical_storage_charge, historical_storage_percent,
        historical_supplier_final_settlement, historical_supplier_price,
    };

    #[test]
    fn supported_profile_ids_are_closed_and_unique() {
        let ids = HistoricalProfile::ALL.map(HistoricalProfile::id);
        let unique = ids.into_iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), 7);
        for profile in HistoricalProfile::ALL {
            assert_eq!(HistoricalProfile::from_id(profile.id()), Some(profile));
        }
        assert_eq!(
            HistoricalProfile::from_id("tac-scm-research-hf-all-v1"),
            None
        );
    }

    #[test]
    fn rounded_uniform_category_weights_are_exact() {
        let loan = (0..12)
            .map(|sample| historical_loan_percent(sample).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(loan, vec![6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12]);
        let storage = (0..50)
            .map(|sample| historical_storage_percent(sample).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(storage.first(), Some(&25));
        assert_eq!(storage.last(), Some(&50));
        for percent in 25..=50 {
            let expected = usize::from(percent != 25 && percent != 50) + 1;
            assert_eq!(
                storage.iter().filter(|value| **value == percent).count(),
                expected
            );
        }
    }

    #[test]
    fn signed_finance_rounding_has_math_round_half_asymmetry() {
        assert_eq!(historical_finance_balance(500_000, 0).unwrap(), 1_000_000);
        assert_eq!(historical_finance_balance(-500_000, 0).unwrap(), 0);
        assert_eq!(
            historical_finance_balance(1_000_000_000, 11).unwrap(),
            1_001_000_000
        );
        assert_eq!(
            historical_finance_balance(-1_000_000_000, 11).unwrap(),
            -1_000_000_000
        );
    }

    #[test]
    fn storage_bounds_agree_on_semantic_fixtures() {
        assert_eq!(historical_storage_charge(0, 25).unwrap(), 0);
        assert_eq!(historical_storage_charge(1_000_000, 25).unwrap(), 0);
        assert_eq!(
            historical_storage_charge(1_000_000_000_000, 25).unwrap(),
            1_015_000_000
        );
        assert_eq!(
            historical_storage_charge(1_000_000_000_000, 50).unwrap(),
            1_845_000_000
        );
        for percent in 25..=50 {
            historical_storage_charge(9_000_000_000_000_000, percent).unwrap();
        }
    }

    #[test]
    fn compound_storage_root_differs_from_simple_annual_division() {
        let inventory = 250_000_000_u128;
        let compound = historical_storage_charge(inventory, 50).unwrap();
        let simple = ((inventory * 50 + 11_000) / 22_000) as i64;
        assert_eq!(compound, 0);
        assert_eq!(simple, 568_182);
        assert_ne!(compound, simple);
    }

    #[test]
    fn historical_terminal_payment_retains_full_down_payment() {
        assert_eq!(
            historical_supplier_final_settlement(4, 100, 100).unwrap(),
            (100, 0, 300)
        );
        assert_eq!(
            historical_supplier_final_settlement(1, 50, 100).unwrap(),
            (100, 0, 0)
        );
    }

    #[test]
    fn supplier_price_responds_to_pressure_and_reputation() {
        let quiet = historical_supplier_price(1_000_000, 100, 0, 10, 0, 100, 1_000_000).unwrap();
        let pressure =
            historical_supplier_price(1_000_000, 100, 0, 180, 1, 100, 1_000_000).unwrap();
        let poor_reputation = historical_supplier_price(1_000_000, 100, 0, 10, 0, 100, 0).unwrap();
        assert!(pressure > quiet);
        assert!(poor_reputation > quiet);
    }
}
