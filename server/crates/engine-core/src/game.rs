//! Complete headless 220-day economic state machine for the draft-2 profile
//! and its explicitly selected draft-3 rule-sync successor.
//!
//! The module is deliberately transport-free. It accepts one already-selected
//! action or declared default per manufacturer at each cutoff.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use sha2::{Digest, Sha256};

use crate::arithmetic::{
    AllocationRequest, ArithmeticError, customer_trend, daily_interest, mul_rate, storage_charge,
    supplier_capacity, supplier_final_settlement, supplier_posted_price,
};
use crate::cbor::Value;
use crate::crypto::{hex, sha256, sha256_value};
use crate::historical::{
    HistoricalError, HistoricalFactors, HistoricalProfile, RESEARCH_PROFILE_REVISION,
    historical_finance_balance, historical_loan_percent, historical_storage_charge,
    historical_storage_percent, historical_supplier_final_settlement, historical_supplier_price,
    percent_to_ppb,
};
use crate::random::{NamedStreams, RandomDraw, RandomError};
use crate::{
    AGENT_IDS, CONFORMANCE_REVISION, CONTRACTS_REVISION, DRAFT3_ENGINE_SEMANTIC_ID,
    DRAFT3_PROFILE_ID, DRAFT3_PROFILE_REVISION, ENGINE_SEMANTIC_ID, EVENT_SCHEMA_ID, PROFILE_ID,
    PROFILE_REVISION, SUPPLIER_MODULE_REVISION,
};

pub const DAYS: u32 = 220;
pub const FACTORY_CYCLES: u32 = 2_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentSpec {
    pub id: &'static str,
    pub base_microcredits: i64,
    pub suppliers: &'static [&'static str],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductSpec {
    pub id: &'static str,
    pub components: [&'static str; 4],
    pub cycles: u32,
    pub segment: &'static str,
}

pub const COMPONENTS: [ComponentSpec; 10] = [
    ComponentSpec {
        id: "cpu-pintel-2",
        base_microcredits: 1_000_000_000,
        suppliers: &["pintel"],
    },
    ComponentSpec {
        id: "cpu-pintel-5",
        base_microcredits: 1_500_000_000,
        suppliers: &["pintel"],
    },
    ComponentSpec {
        id: "cpu-imd-2",
        base_microcredits: 1_000_000_000,
        suppliers: &["imd"],
    },
    ComponentSpec {
        id: "cpu-imd-5",
        base_microcredits: 1_500_000_000,
        suppliers: &["imd"],
    },
    ComponentSpec {
        id: "board-pintel",
        base_microcredits: 250_000_000,
        suppliers: &["basus", "macrostar"],
    },
    ComponentSpec {
        id: "board-imd",
        base_microcredits: 250_000_000,
        suppliers: &["basus", "macrostar"],
    },
    ComponentSpec {
        id: "memory-1",
        base_microcredits: 100_000_000,
        suppliers: &["mec", "queenmax"],
    },
    ComponentSpec {
        id: "memory-2",
        base_microcredits: 200_000_000,
        suppliers: &["mec", "queenmax"],
    },
    ComponentSpec {
        id: "disk-300",
        base_microcredits: 300_000_000,
        suppliers: &["watergate", "mintor"],
    },
    ComponentSpec {
        id: "disk-500",
        base_microcredits: 400_000_000,
        suppliers: &["watergate", "mintor"],
    },
];

pub const PRODUCTS: [ProductSpec; 16] = [
    ProductSpec {
        id: "p01",
        components: ["cpu-pintel-2", "board-pintel", "memory-1", "disk-300"],
        cycles: 4,
        segment: "low",
    },
    ProductSpec {
        id: "p02",
        components: ["cpu-pintel-2", "board-pintel", "memory-1", "disk-500"],
        cycles: 5,
        segment: "low",
    },
    ProductSpec {
        id: "p03",
        components: ["cpu-pintel-2", "board-pintel", "memory-2", "disk-300"],
        cycles: 5,
        segment: "mid",
    },
    ProductSpec {
        id: "p04",
        components: ["cpu-pintel-2", "board-pintel", "memory-2", "disk-500"],
        cycles: 6,
        segment: "mid",
    },
    ProductSpec {
        id: "p05",
        components: ["cpu-pintel-5", "board-pintel", "memory-1", "disk-300"],
        cycles: 5,
        segment: "mid",
    },
    ProductSpec {
        id: "p06",
        components: ["cpu-pintel-5", "board-pintel", "memory-1", "disk-500"],
        cycles: 6,
        segment: "high",
    },
    ProductSpec {
        id: "p07",
        components: ["cpu-pintel-5", "board-pintel", "memory-2", "disk-300"],
        cycles: 6,
        segment: "high",
    },
    ProductSpec {
        id: "p08",
        components: ["cpu-pintel-5", "board-pintel", "memory-2", "disk-500"],
        cycles: 7,
        segment: "high",
    },
    ProductSpec {
        id: "p09",
        components: ["cpu-imd-2", "board-imd", "memory-1", "disk-300"],
        cycles: 4,
        segment: "low",
    },
    ProductSpec {
        id: "p10",
        components: ["cpu-imd-2", "board-imd", "memory-1", "disk-500"],
        cycles: 5,
        segment: "low",
    },
    ProductSpec {
        id: "p11",
        components: ["cpu-imd-2", "board-imd", "memory-2", "disk-300"],
        cycles: 5,
        segment: "low",
    },
    ProductSpec {
        id: "p12",
        components: ["cpu-imd-2", "board-imd", "memory-2", "disk-500"],
        cycles: 6,
        segment: "mid",
    },
    ProductSpec {
        id: "p13",
        components: ["cpu-imd-5", "board-imd", "memory-1", "disk-300"],
        cycles: 5,
        segment: "mid",
    },
    ProductSpec {
        id: "p14",
        components: ["cpu-imd-5", "board-imd", "memory-1", "disk-500"],
        cycles: 6,
        segment: "mid",
    },
    ProductSpec {
        id: "p15",
        components: ["cpu-imd-5", "board-imd", "memory-2", "disk-300"],
        cycles: 6,
        segment: "high",
    },
    ProductSpec {
        id: "p16",
        components: ["cpu-imd-5", "board-imd", "memory-2", "disk-500"],
        cycles: 7,
        segment: "high",
    },
];

/// Rule switches that distinguish the normative profiles. Draft 2
/// and every research profile use `DRAFT2`; draft 3 enables all five.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileRules {
    /// A completely delivered supplier order is reported in one observation
    /// and then removed, as a settled customer order is.
    pub retire_delivered_supplier_orders: bool,
    /// At most five positive RFQs and one probe per supplier-component pair
    /// and day; every RFQ of an exceeded class is rejected as `cardinality`.
    pub supplier_rfq_pair_limit: bool,
    /// A customer RFQ accepts offers only in the action window of its issue
    /// day; unawarded RFQs are removed at the next customer settlement.
    pub issue_day_customer_bids: bool,
    /// Customers issue no RFQs on day 0; the demand walk still advances.
    pub no_day_zero_customer_rfqs: bool,
    /// At most one delivery line per customer order and action; a later
    /// line for an order already scheduled is rejected as `cardinality`.
    pub one_delivery_line_per_order: bool,
}

impl ProfileRules {
    pub const DRAFT2: Self = Self {
        retire_delivered_supplier_orders: false,
        supplier_rfq_pair_limit: false,
        issue_day_customer_bids: false,
        no_day_zero_customer_rfqs: false,
        one_delivery_line_per_order: false,
    };
    pub const DRAFT3: Self = Self {
        retire_delivered_supplier_orders: true,
        supplier_rfq_pair_limit: true,
        issue_day_customer_bids: true,
        no_day_zero_customer_rfqs: true,
        one_delivery_line_per_order: true,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GamePlan {
    pub game_id: String,
    pub root_seed: [u8; 32],
    pub participants: [String; 6],
    pub profile_id: String,
    pub profile_revision: u32,
    pub supplier_module_revision: u32,
    pub contracts_revision: u32,
    pub conformance_revision: u32,
    pub loan_rate_ppb: u32,
    pub deposit_rate_ppb: u32,
    pub storage_rate_ppb: u32,
    pub initial_means_nano: [u64; 3],
    /// Canonical pattern registry; instantiated positions live in state.
    pub stream_patterns: Vec<String>,
    pub engine_identity: [u8; 32],
    pub configuration_identity: [u8; 32],
    /// Optional outer frozen-manifest identity. The runner binds this only
    /// after hashing the complete operational plan; it is not semantic state.
    pub external_plan_identity: Option<[u8; 32]>,
}

impl GamePlan {
    pub fn materialize(root_seed: [u8; 32]) -> Result<Self, GameError> {
        let participants = AGENT_IDS.map(str::to_owned);
        let mut streams = NamedStreams::new(root_seed, PROFILE_ID);
        let (loan_bps, _) = streams.uniform_u64("config/loan-rate", 600, 1_200)?;
        let (storage_bps, _) = streams.uniform_u64("config/storage-rate", 2_500, 5_000)?;
        let (low, _) = streams.uniform_u64("customer/low/initial-mean", 25, 100)?;
        let (mid, _) = streams.uniform_u64("customer/mid/initial-mean", 30, 120)?;
        let (high, _) = streams.uniform_u64("customer/high/initial-mean", 25, 100)?;
        let loan_rate_ppb = u32::try_from(loan_bps * 100_000).expect("profile rate fits u32");
        let storage_rate_ppb = u32::try_from(storage_bps * 100_000).expect("profile rate fits u32");
        let deposit_rate_ppb = loan_rate_ppb / 2;
        let initial_means_nano = [
            low * 1_000_000_000,
            mid * 1_000_000_000,
            high * 1_000_000_000,
        ];
        let stream_patterns = vec![
            "config/{loan-rate,storage-rate}".into(),
            "supplier/{supplier}/{component}/{initial-capacity,capacity/{day}}".into(),
            "customer/{segment}/{initial-mean,trend/{day},count/{day}}".into(),
            "customer/{rfq-id}/{product,quantity,lead-time,reserve,penalty}".into(),
            "symmetry/{day}/{phase}/{decision}/{context}/rank/{rank}".into(),
        ];
        let engine_identity = sha256(ENGINE_SEMANTIC_ID.as_bytes());
        let config_value = Value::Map(vec![
            ("profile".into(), Value::Text(PROFILE_ID.into())),
            (
                "participants".into(),
                Value::Array(participants.iter().cloned().map(Value::Text).collect()),
            ),
            ("days".into(), Value::U64(u64::from(DAYS))),
            (
                "factory_cycles".into(),
                Value::U64(u64::from(FACTORY_CYCLES)),
            ),
            ("loan_rate_ppb".into(), Value::U64(u64::from(loan_rate_ppb))),
            (
                "deposit_rate_ppb".into(),
                Value::U64(u64::from(deposit_rate_ppb)),
            ),
            (
                "storage_rate_ppb".into(),
                Value::U64(u64::from(storage_rate_ppb)),
            ),
            (
                "initial_means_nano".into(),
                Value::Array(initial_means_nano.iter().copied().map(Value::U64).collect()),
            ),
            (
                "root_seed_commitment".into(),
                Value::Bytes(sha256(&root_seed).to_vec()),
            ),
            (
                "stream_patterns".into(),
                Value::Array(stream_patterns.iter().cloned().map(Value::Text).collect()),
            ),
            (
                "engine_identity".into(),
                Value::Bytes(engine_identity.to_vec()),
            ),
        ]);
        let configuration_identity = sha256(&config_value.encode());
        let game_id = format!(
            "plan/{}",
            hex(&sha256(
                &Value::Map(vec![
                    (
                        "configuration_identity".into(),
                        Value::Bytes(configuration_identity.to_vec())
                    ),
                    (
                        "engine_identity".into(),
                        Value::Bytes(engine_identity.to_vec())
                    ),
                    ("run_nonce".into(), Value::Bytes([0_u8; 16].to_vec())),
                ])
                .encode()
            ))
        );
        Ok(Self {
            game_id,
            root_seed,
            participants,
            profile_id: PROFILE_ID.into(),
            profile_revision: PROFILE_REVISION,
            supplier_module_revision: SUPPLIER_MODULE_REVISION,
            contracts_revision: CONTRACTS_REVISION,
            conformance_revision: CONFORMANCE_REVISION,
            loan_rate_ppb,
            deposit_rate_ppb,
            storage_rate_ppb,
            initial_means_nano,
            stream_patterns,
            engine_identity,
            configuration_identity,
            external_plan_identity: None,
        })
    }

    /// Materialize the explicitly selected draft-3 rule-sync profile. It keeps
    /// draft 2's random-stream namespace and stream registry, so one root seed
    /// gives the same exogenous configuration and market draws under both
    /// revisions; only the profile, its revision, and the engine semantic
    /// identity (and therefore the configuration identity and game id) differ.
    pub fn materialize_draft3(root_seed: [u8; 32]) -> Result<Self, GameError> {
        let mut plan = Self::materialize(root_seed)?;
        plan.profile_id = DRAFT3_PROFILE_ID.into();
        plan.profile_revision = DRAFT3_PROFILE_REVISION;
        plan.engine_identity = sha256(DRAFT3_ENGINE_SEMANTIC_ID.as_bytes());
        plan.configuration_identity = research_configuration_identity(&plan);
        plan.game_id = configuration_game_id(plan.configuration_identity, plan.engine_identity);
        Ok(plan)
    }

    /// Materialize any profile this engine runs by its id: the default draft
    /// 2, draft 3, or one research-only historical profile.
    pub fn materialize_profile(root_seed: [u8; 32], profile_id: &str) -> Result<Self, GameError> {
        if profile_id == PROFILE_ID {
            Self::materialize(root_seed)
        } else if profile_id == DRAFT3_PROFILE_ID {
            Self::materialize_draft3(root_seed)
        } else {
            let profile = HistoricalProfile::from_id(profile_id)
                .ok_or(GameError::ConfigurationInvalid("unknown profile id"))?;
            Self::materialize_research(root_seed, profile)
        }
    }

    #[must_use]
    pub fn rules(&self) -> ProfileRules {
        if self.profile_id == DRAFT3_PROFILE_ID {
            ProfileRules::DRAFT3
        } else {
            ProfileRules::DRAFT2
        }
    }

    /// Materialize one closed research-only historical-rule profile while
    /// retaining the normative profile's common exogenous named streams.
    pub fn materialize_research(
        root_seed: [u8; 32],
        profile: HistoricalProfile,
    ) -> Result<Self, GameError> {
        let mut plan = Self::materialize(root_seed)?;
        plan.profile_id = profile.id().into();
        plan.profile_revision = RESEARCH_PROFILE_REVISION;
        if profile.factors().financial_carry {
            let mut streams = NamedStreams::new(root_seed, PROFILE_ID);
            let (loan_sample, _) =
                streams.uniform_u64("historical/financial/loan-half-step", 0, 11)?;
            let (storage_sample, _) =
                streams.uniform_u64("historical/financial/storage-half-step", 0, 49)?;
            let loan_percent = historical_loan_percent(loan_sample)?;
            let storage_percent = historical_storage_percent(storage_sample)?;
            plan.loan_rate_ppb = percent_to_ppb(loan_percent);
            plan.deposit_rate_ppb = percent_to_ppb(loan_percent / 2);
            plan.storage_rate_ppb = percent_to_ppb(storage_percent);
            plan.stream_patterns
                .push("historical/financial/{loan-half-step,storage-half-step}".into());
        }
        plan.configuration_identity = research_configuration_identity(&plan);
        plan.game_id = configuration_game_id(plan.configuration_identity, plan.engine_identity);
        Ok(plan)
    }

    #[must_use]
    pub fn historical_profile(&self) -> Option<HistoricalProfile> {
        HistoricalProfile::from_id(&self.profile_id)
    }

    #[must_use]
    pub fn historical_factors(&self) -> HistoricalFactors {
        self.historical_profile()
            .map_or(HistoricalFactors::NONE, HistoricalProfile::factors)
    }

    /// Bind the complete outer manifest identity without changing semantic
    /// configuration. The digest is checked by `validate`.
    #[must_use]
    pub fn bind_external_plan(mut self, digest: [u8; 32]) -> Self {
        self.game_id = format!("plan/{}", hex(&digest));
        self.external_plan_identity = Some(digest);
        self
    }

    pub fn validate(&self) -> Result<(), GameError> {
        let historical_profile = self.historical_profile();
        let profile_valid = if self.profile_id == PROFILE_ID {
            self.profile_revision == PROFILE_REVISION
        } else if self.profile_id == DRAFT3_PROFILE_ID {
            self.profile_revision == DRAFT3_PROFILE_REVISION
        } else {
            historical_profile.is_some() && self.profile_revision == RESEARCH_PROFILE_REVISION
        };
        if !profile_valid
            || self.supplier_module_revision != SUPPLIER_MODULE_REVISION
            || self.contracts_revision != CONTRACTS_REVISION
            || self.conformance_revision != CONFORMANCE_REVISION
            || self.participants != AGENT_IDS.map(str::to_owned)
            || self.engine_identity
                != sha256(if self.profile_id == DRAFT3_PROFILE_ID {
                    DRAFT3_ENGINE_SEMANTIC_ID.as_bytes()
                } else {
                    ENGINE_SEMANTIC_ID.as_bytes()
                })
        {
            return Err(GameError::ConfigurationInvalid(
                "profile/module/participant identity",
            ));
        }
        let expected = Self::materialize_profile(self.root_seed, &self.profile_id)?;
        let game_id_valid = self.external_plan_identity.map_or_else(
            || self.game_id == expected.game_id,
            |digest| self.game_id == format!("plan/{}", hex(&digest)),
        );
        if self.configuration_identity != expected.configuration_identity
            || !game_id_valid
            || self.loan_rate_ppb != expected.loan_rate_ppb
            || self.deposit_rate_ppb != expected.deposit_rate_ppb
            || self.storage_rate_ppb != expected.storage_rate_ppb
            || self.initial_means_nano != expected.initial_means_nano
            || self.stream_patterns != expected.stream_patterns
        {
            return Err(GameError::ConfigurationInvalid(
                "materialized configuration identity",
            ));
        }
        Ok(())
    }
}

fn research_configuration_identity(plan: &GamePlan) -> [u8; 32] {
    sha256(
        &Value::Map(vec![
            ("profile".into(), Value::Text(plan.profile_id.clone())),
            (
                "participants".into(),
                Value::Array(plan.participants.iter().cloned().map(Value::Text).collect()),
            ),
            ("days".into(), Value::U64(u64::from(DAYS))),
            (
                "factory_cycles".into(),
                Value::U64(u64::from(FACTORY_CYCLES)),
            ),
            (
                "loan_rate_ppb".into(),
                Value::U64(u64::from(plan.loan_rate_ppb)),
            ),
            (
                "deposit_rate_ppb".into(),
                Value::U64(u64::from(plan.deposit_rate_ppb)),
            ),
            (
                "storage_rate_ppb".into(),
                Value::U64(u64::from(plan.storage_rate_ppb)),
            ),
            (
                "initial_means_nano".into(),
                Value::Array(
                    plan.initial_means_nano
                        .iter()
                        .copied()
                        .map(Value::U64)
                        .collect(),
                ),
            ),
            (
                "root_seed_commitment".into(),
                Value::Bytes(sha256(&plan.root_seed).to_vec()),
            ),
            (
                "stream_patterns".into(),
                Value::Array(
                    plan.stream_patterns
                        .iter()
                        .cloned()
                        .map(Value::Text)
                        .collect(),
                ),
            ),
            (
                "engine_identity".into(),
                Value::Bytes(plan.engine_identity.to_vec()),
            ),
        ])
        .encode(),
    )
}

fn configuration_game_id(configuration_identity: [u8; 32], engine_identity: [u8; 32]) -> String {
    format!(
        "plan/{}",
        hex(&sha256(
            &Value::Map(vec![
                (
                    "configuration_identity".into(),
                    Value::Bytes(configuration_identity.to_vec()),
                ),
                (
                    "engine_identity".into(),
                    Value::Bytes(engine_identity.to_vec()),
                ),
                ("run_nonce".into(), Value::Bytes([0_u8; 16].to_vec())),
            ])
            .encode(),
        ))
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerOffer {
    pub command_id: String,
    pub rfq_id: String,
    pub unit_price: i64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierRfqCommand {
    pub command_id: String,
    pub rfq_id: String,
    pub supplier_id: String,
    pub component_id: String,
    pub quantity: u32,
    pub due_day: u32,
    pub max_unit_price: Option<i64>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierOrderCommand {
    pub command_id: String,
    pub offer_id: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionLine {
    pub line_id: String,
    pub priority: u32,
    pub product_id: String,
    pub quantity: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryLine {
    pub line_id: String,
    pub priority: u32,
    pub customer_order_id: String,
    pub quantity: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameActionSet {
    pub agent_id: String,
    pub observation_id: String,
    pub day: u32,
    pub revision: u32,
    pub customer_offers: Vec<CustomerOffer>,
    pub supplier_rfqs: Vec<SupplierRfqCommand>,
    pub supplier_orders: Vec<SupplierOrderCommand>,
    pub production_schedule: Vec<ProductionLine>,
    pub delivery_schedule: Vec<DeliveryLine>,
}

impl GameActionSet {
    #[must_use]
    pub fn empty(agent_id: &str, observation_id: &str, day: u32, revision: u32) -> Self {
        Self {
            agent_id: agent_id.into(),
            observation_id: observation_id.into(),
            day,
            revision,
            customer_offers: Vec::new(),
            supplier_rfqs: Vec::new(),
            supplier_orders: Vec::new(),
            production_schedule: Vec::new(),
            delivery_schedule: Vec::new(),
        }
    }

    /// Canonical logical action used at the transport and replay boundaries.
    #[must_use]
    pub fn value(&self) -> Value {
        Value::Map(vec![
            ("type".into(), Value::Text("day-action-set".into())),
            ("agent_id".into(), Value::Text(self.agent_id.clone())),
            (
                "observation_id".into(),
                Value::Text(self.observation_id.clone()),
            ),
            ("day".into(), Value::U64(u64::from(self.day))),
            ("revision".into(), Value::U64(u64::from(self.revision))),
            (
                "customer_offers".into(),
                Value::Array(
                    self.customer_offers
                        .iter()
                        .map(|x| {
                            Value::Map(vec![
                                ("command_id".into(), Value::Text(x.command_id.clone())),
                                ("rfq_id".into(), Value::Text(x.rfq_id.clone())),
                                ("unit_price".into(), Value::I64(x.unit_price)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "supplier_rfqs".into(),
                Value::Array(
                    self.supplier_rfqs
                        .iter()
                        .map(|x| {
                            Value::Map(vec![
                                ("command_id".into(), Value::Text(x.command_id.clone())),
                                ("rfq_id".into(), Value::Text(x.rfq_id.clone())),
                                ("supplier_id".into(), Value::Text(x.supplier_id.clone())),
                                ("component_id".into(), Value::Text(x.component_id.clone())),
                                ("quantity".into(), Value::U64(u64::from(x.quantity))),
                                ("due_day".into(), Value::U64(u64::from(x.due_day))),
                                (
                                    "max_unit_price".into(),
                                    x.max_unit_price.map_or(Value::Null, Value::I64),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "supplier_orders".into(),
                Value::Array(
                    self.supplier_orders
                        .iter()
                        .map(|x| {
                            Value::Map(vec![
                                ("command_id".into(), Value::Text(x.command_id.clone())),
                                ("offer_id".into(), Value::Text(x.offer_id.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "production_schedule".into(),
                Value::Array(
                    self.production_schedule
                        .iter()
                        .map(|x| {
                            Value::Map(vec![
                                ("line_id".into(), Value::Text(x.line_id.clone())),
                                ("priority".into(), Value::U64(u64::from(x.priority))),
                                ("product_id".into(), Value::Text(x.product_id.clone())),
                                ("quantity".into(), Value::U64(u64::from(x.quantity))),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "delivery_schedule".into(),
                Value::Array(
                    self.delivery_schedule
                        .iter()
                        .map(|x| {
                            Value::Map(vec![
                                ("line_id".into(), Value::Text(x.line_id.clone())),
                                ("priority".into(), Value::U64(u64::from(x.priority))),
                                (
                                    "customer_order_id".into(),
                                    Value::Text(x.customer_order_id.clone()),
                                ),
                                ("quantity".into(), Value::U64(u64::from(x.quantity))),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultCause {
    Missing,
    Timeout,
    ProcessExit,
    ResourceLimit,
    TransportError,
}

impl DefaultCause {
    fn text(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Timeout => "timeout",
            Self::ProcessExit => "process-exit",
            Self::ResourceLimit => "resource-limit",
            Self::TransportError => "transport-error",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionInput {
    Accepted(GameActionSet),
    Default {
        agent_id: String,
        cause: DefaultCause,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerRfq {
    pub id: String,
    pub product_id: String,
    pub quantity: u32,
    pub due_day: u32,
    pub reserve: i64,
    pub penalty: i64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CustomerOrderStatus {
    Open,
    DeliveredPending { delivery_day: u32 },
    Delivered,
    Cancelled,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerOrder {
    pub id: String,
    pub rfq_id: String,
    pub agent_id: String,
    pub product_id: String,
    pub quantity: u32,
    pub due_day: u32,
    pub unit_price: i64,
    pub penalty: i64,
    pub penalty_count: u8,
    pub status: CustomerOrderStatus,
    pub delivery_day: Option<u32>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupplierOfferKind {
    OnDate,
    Complete,
    Probe,
    None,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierOffer {
    pub id: String,
    pub group_id: String,
    pub rfq_id: String,
    pub agent_id: String,
    pub supplier_id: String,
    pub component_id: String,
    pub kind: SupplierOfferKind,
    pub quantity: u32,
    pub due_day: u32,
    pub unit_price: i64,
    pub valid_day: u32,
    pub no_offer_reason: Option<&'static str>,
    /// Reputation exposure before any complete alternative is purchased.
    pub exposure: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierOrder {
    pub id: String,
    pub offer_id: String,
    pub agent_id: String,
    pub supplier_id: String,
    pub component_id: String,
    pub quantity: u32,
    pub due_day: u32,
    pub unit_price: i64,
    pub down_payment: i64,
    pub delivered: u32,
    pub ordinal: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Segment {
    id: &'static str,
    q_nano: u64,
    trend_ppb: u64,
    min_nano: u64,
    max_nano: u64,
    next_ordinal: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Reputation {
    offered: u64,
    purchased: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct SupplierLine {
    supplier_id: String,
    component_id: String,
    actual_capacity: u32,
    inventory: u32,
    next_ordinal: u64,
    orders: Vec<SupplierOrder>,
    offers: Vec<SupplierOffer>,
    reputations: BTreeMap<String, Reputation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentState {
    pub id: String,
    pub cash: i64,
    pub components: BTreeMap<String, u32>,
    pub pcs: BTreeMap<String, u32>,
    pub production_schedule: Vec<ProductionLine>,
    pub delivery_schedule: Vec<DeliveryLine>,
    pub customer_orders: Vec<CustomerOrder>,
    pub supplier_rfqs: Vec<SupplierRfq>,
    pub production_results: Vec<ScheduleResult>,
    pub delivery_results: Vec<ScheduleResult>,
    pub last_observation_id: Option<String>,
    pub defaults: Vec<(u32, DefaultCause)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleResult {
    pub id: String,
    pub agent_id: String,
    pub schedule_kind: &'static str,
    pub line_id: String,
    pub executed: bool,
    pub quantity: u32,
    pub rejection: Option<CommandRejection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierRfq {
    pub id: String,
    pub client_rfq_id: String,
    pub agent_id: String,
    pub supplier_id: String,
    pub component_id: String,
    pub quantity: u32,
    pub due_day: u32,
    pub max_unit_price: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierDelivery {
    pub id: String,
    pub order_id: String,
    pub component_id: String,
    pub quantity: u32,
    pub unit_price: i64,
    pub invoice: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplierReputation {
    pub id: String,
    pub supplier_id: String,
    pub component_id: String,
    pub agent_id: String,
    pub offered: u64,
    pub purchased: u64,
    pub reputation_ppm: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerSettlement {
    pub id: String,
    pub order_id: String,
    pub delivered: bool,
    pub payment: i64,
    pub penalty: i64,
    pub penalty_count: u8,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerTransaction {
    pub id: String,
    pub day: u32,
    pub phase: &'static str,
    pub source_account: String,
    pub destination_account: String,
    pub amount: i64,
    pub reason: String,
    pub cause_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionDisposition {
    pub day: u32,
    pub action_digest: [u8; 32],
    pub cause: &'static str,
    pub revision: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyPriceRange {
    pub product_id: String,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Observation {
    pub agent_id: String,
    pub day: u32,
    pub id: String,
    pub state_commitment: [u8; 32],
    pub event_cursor: u64,
    pub cash: i64,
    pub ledger_since_previous: Vec<LedgerTransaction>,
    pub component_inventory: BTreeMap<String, u32>,
    pub pc_inventory: BTreeMap<String, u32>,
    pub pending_components: BTreeMap<String, u32>,
    pub pending_pcs: BTreeMap<String, u32>,
    pub production_results: Vec<ScheduleResult>,
    pub delivery_results: Vec<ScheduleResult>,
    pub supplier_rfqs: Vec<SupplierRfq>,
    pub customer_rfqs: Vec<CustomerRfq>,
    pub new_customer_orders: Vec<CustomerOrder>,
    pub customer_orders: Vec<CustomerOrder>,
    pub supplier_offers: Vec<SupplierOffer>,
    pub supplier_orders: Vec<SupplierOrder>,
    pub supplier_deliveries: Vec<SupplierDelivery>,
    pub supplier_reputations: Vec<SupplierReputation>,
    pub customer_settlements: Vec<CustomerSettlement>,
    pub previous_action: Option<ActionDisposition>,
    pub daily_prices: Vec<DailyPriceRange>,
    pub default_history: Vec<(u32, DefaultCause)>,
    pub rejections: Vec<CommandRejection>,
    pub market_report: Option<MarketReport>,
}

impl Observation {
    /// Complete normative day-observation payload for the runner/SDK boundary.
    #[must_use]
    pub fn value(&self) -> Value {
        let mut pending = inventory_values(
            &self.agent_id,
            "pending-component",
            &self.pending_components,
        );
        pending.extend(inventory_values(
            &self.agent_id,
            "pending-pc",
            &self.pending_pcs,
        ));
        Value::Map(vec![
            ("type".into(), Value::Text("day-observation".into())),
            ("observation_id".into(), Value::Text(self.id.clone())),
            ("day".into(), Value::U64(u64::from(self.day))),
            (
                "state_commitment".into(),
                Value::Bytes(self.state_commitment.to_vec()),
            ),
            ("event_cursor".into(), Value::U64(self.event_cursor)),
            ("cash_microcredits".into(), Value::I64(self.cash)),
            (
                "ledger_since_previous".into(),
                Value::Array(
                    self.ledger_since_previous
                        .iter()
                        .map(ledger_transaction_value)
                        .collect(),
                ),
            ),
            (
                "component_inventory".into(),
                Value::Array(inventory_values(
                    &self.agent_id,
                    "component",
                    &self.component_inventory,
                )),
            ),
            (
                "pc_inventory".into(),
                Value::Array(inventory_values(&self.agent_id, "pc", &self.pc_inventory)),
            ),
            ("pending_inventory".into(), Value::Array(pending)),
            (
                "production_results".into(),
                Value::Array(
                    self.production_results
                        .iter()
                        .map(schedule_result_value)
                        .collect(),
                ),
            ),
            (
                "delivery_results".into(),
                Value::Array(
                    self.delivery_results
                        .iter()
                        .map(schedule_result_value)
                        .collect(),
                ),
            ),
            (
                "supplier_rfqs".into(),
                Value::Array(self.supplier_rfqs.iter().map(supplier_rfq_value).collect()),
            ),
            (
                "supplier_offers".into(),
                Value::Array(
                    self.supplier_offers
                        .iter()
                        .map(supplier_offer_value)
                        .collect(),
                ),
            ),
            (
                "supplier_orders".into(),
                Value::Array(
                    self.supplier_orders
                        .iter()
                        .map(|order| supplier_order_value(order, &order.supplier_id))
                        .collect(),
                ),
            ),
            (
                "supplier_deliveries".into(),
                Value::Array(
                    self.supplier_deliveries
                        .iter()
                        .map(supplier_delivery_value)
                        .collect(),
                ),
            ),
            (
                "supplier_reputations".into(),
                Value::Array(
                    self.supplier_reputations
                        .iter()
                        .map(supplier_reputation_value)
                        .collect(),
                ),
            ),
            (
                "customer_rfqs".into(),
                Value::Array(self.customer_rfqs.iter().map(customer_rfq_value).collect()),
            ),
            (
                "new_customer_orders".into(),
                Value::Array(
                    self.new_customer_orders
                        .iter()
                        .map(|order| customer_order_value(order, self.day))
                        .collect(),
                ),
            ),
            (
                "active_customer_orders".into(),
                Value::Array(
                    self.customer_orders
                        .iter()
                        .map(|order| customer_order_value(order, self.day))
                        .collect(),
                ),
            ),
            (
                "customer_settlements".into(),
                Value::Array(
                    self.customer_settlements
                        .iter()
                        .map(customer_settlement_value)
                        .collect(),
                ),
            ),
            (
                "previous_action".into(),
                self.previous_action
                    .as_ref()
                    .map_or(Value::Null, action_disposition_value),
            ),
            (
                "daily_price_report".into(),
                daily_price_report_value(self.day, &self.daily_prices),
            ),
            (
                "market_report".into(),
                self.market_report
                    .as_ref()
                    .map_or(Value::Null, market_report_value),
            ),
            (
                "default_history".into(),
                Value::Array(
                    self.default_history
                        .iter()
                        .map(|(day, cause)| {
                            Value::Map(vec![
                                ("day".into(), Value::U64(u64::from(*day))),
                                ("cause".into(), Value::Text(cause.text().into())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRejection {
    pub command_id: String,
    pub code: &'static str,
    pub field_path: String,
    pub detail: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductMarketStats {
    pub product_id: String,
    pub rfq_count: u64,
    pub rfq_quantity: u64,
    pub order_count: u64,
    pub order_quantity: u64,
    pub minimum_winning_unit_price: Option<i64>,
    pub maximum_winning_unit_price: Option<i64>,
    pub mean_winning_unit_price: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentMarketStats {
    pub component_id: String,
    pub ordered_quantity: u64,
    pub delivered_quantity: u64,
    pub mean_ordered_unit_price: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketReport {
    pub start_day: u32,
    pub end_day: u32,
    pub products: Vec<ProductMarketStats>,
    pub components: Vec<ComponentMarketStats>,
    pub supplier_lines: Vec<CapacityMarketStats>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapacityMarketStats {
    pub supplier_id: String,
    pub component_id: String,
    pub mean_capacity_nano: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ProductAccumulator {
    rfq_count: u64,
    rfq_quantity: u64,
    order_count: u64,
    order_quantity: u64,
    winning_price_sum: i128,
    winning_price_count: u64,
    winning_min: Option<i64>,
    winning_max: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ComponentAccumulator {
    ordered_quantity: u64,
    delivered_quantity: u64,
    ordered_price_sum: i128,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CapacityAccumulator {
    capacity_sum: u64,
    capacity_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameEvent {
    pub index: u64,
    pub event_type: &'static str,
    pub day: Option<u32>,
    pub phase: &'static str,
    pub previous_hash: [u8; 32],
    pub state_commitment: Option<[u8; 32]>,
    pub hash: [u8; 32],
    pub canonical_bytes: Vec<u8>,
    pub visibility: EventVisibility,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventVisibility {
    Public,
    Private(String),
    Operator,
}

enum AgentTransfer<'a> {
    Credit(&'a str),
    Debit(&'a str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameResult {
    pub transcript_bytes: Vec<u8>,
    pub transcript_sha256: [u8; 32],
    pub transcript_offsets: Vec<usize>,
    pub event_log_bytes: Vec<u8>,
    pub event_log_sha256: [u8; 32],
    pub event_offsets: Vec<usize>,
    pub event_count: u64,
    pub events_captured: bool,
    pub economic_summary_bytes: Vec<u8>,
    pub economic_summary_sha256: [u8; 32],
    pub daily_commitments: Vec<[u8; 32]>,
    pub final_state_sha256: [u8; 32],
    pub scores: Vec<(String, i64)>,
}

type EventSink = Box<dyn FnMut(&[u8]) + Send>;

enum EventOutput {
    Off,
    Capture,
    Stream { sink: EventSink, digest: Sha256 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GameError {
    ConfigurationInvalid(&'static str),
    WrongPhase,
    WrongActionCount(usize),
    InvalidAction {
        agent: String,
        reason: &'static str,
    },
    Arithmetic(ArithmeticError),
    Historical(HistoricalError),
    Random(RandomError),
    ReplayMismatch {
        boundary: &'static str,
        index: Option<usize>,
        byte_offset: Option<usize>,
    },
    Invariant(&'static str),
}
impl fmt::Display for GameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GameError {}
impl From<ArithmeticError> for GameError {
    fn from(value: ArithmeticError) -> Self {
        Self::Arithmetic(value)
    }
}
impl From<RandomError> for GameError {
    fn from(value: RandomError) -> Self {
        Self::Random(value)
    }
}
impl From<HistoricalError> for GameError {
    fn from(value: HistoricalError) -> Self {
        Self::Historical(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Progress {
    Ready,
    Waiting,
}

pub struct GameEngine {
    plan: GamePlan,
    day: u32,
    progress: Progress,
    streams: NamedStreams,
    pub agents: Vec<AgentState>,
    supplier_lines: Vec<SupplierLine>,
    segments: Vec<Segment>,
    customer_rfqs: Vec<CustomerRfq>,
    customer_bids: Vec<(u32, String, CustomerOffer)>,
    staged_deliveries: Vec<(usize, usize, DeliveryLine)>,
    pending_components: Vec<BTreeMap<String, u32>>,
    pending_pcs: Vec<BTreeMap<String, u32>>,
    events: Vec<GameEvent>,
    event_count: u64,
    event_output: EventOutput,
    // Training engine mode: phase state commitments are left at
    // zero; the game's dynamics never read them.
    state_commitments: bool,
    previous_event_hash: [u8; 32],
    transcript: Vec<Value>,
    committed_actions: Vec<Value>,
    daily_commitments: Vec<[u8; 32]>,
    observations: Vec<Observation>,
    rejections: Vec<Vec<CommandRejection>>,
    product_market: BTreeMap<String, ProductAccumulator>,
    component_market: BTreeMap<String, ComponentAccumulator>,
    capacity_market: BTreeMap<(String, String), CapacityAccumulator>,
    market_start_day: u32,
    current_market_report: Option<MarketReport>,
    ledger_transactions: Vec<LedgerTransaction>,
    supplier_deliveries: Vec<Vec<SupplierDelivery>>,
    customer_settlements: Vec<Vec<CustomerSettlement>>,
    new_customer_orders: Vec<Vec<CustomerOrder>>,
    daily_prices: BTreeMap<String, (Option<i64>, Option<i64>)>,
    ledger_accounts: BTreeMap<String, i64>,
    completed: bool,
    entity_ordinals: BTreeMap<&'static str, u64>,
    economic_ordinals: BTreeMap<&'static str, u64>,
    economic_kind_counts: BTreeMap<&'static str, u64>,
    /// Delivered units of supplier orders already retired from state under
    /// draft 3, per agent slot; keeps the economic summary's receipt totals.
    retired_supplier_units: [u64; 6],
}

impl GameEngine {
    pub fn new(plan: GamePlan) -> Result<Self, GameError> {
        Self::new_with_event_output(plan, EventOutput::Off)
    }

    pub fn new_with_event_capture(plan: GamePlan) -> Result<Self, GameError> {
        Self::new_with_event_output(plan, EventOutput::Capture)
    }

    pub fn new_with_event_stream<F>(plan: GamePlan, sink: F) -> Result<Self, GameError>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        Self::new_with_event_output(
            plan,
            EventOutput::Stream {
                sink: Box::new(sink),
                digest: Sha256::new(),
            },
        )
    }

    fn new_with_event_output(plan: GamePlan, event_output: EventOutput) -> Result<Self, GameError> {
        plan.validate()?;
        let mut streams = NamedStreams::new(plan.root_seed, PROFILE_ID);
        // Consume the public materialization streams before economic setup.
        streams.uniform_u64("config/loan-rate", 600, 1_200)?;
        streams.uniform_u64("config/storage-rate", 2_500, 5_000)?;
        streams.uniform_u64("customer/low/initial-mean", 25, 100)?;
        streams.uniform_u64("customer/mid/initial-mean", 30, 120)?;
        streams.uniform_u64("customer/high/initial-mean", 25, 100)?;
        let agents = plan
            .participants
            .iter()
            .map(|id| AgentState {
                id: id.clone(),
                cash: 0,
                components: COMPONENTS
                    .iter()
                    .map(|component| (component.id.into(), 0))
                    .collect(),
                pcs: PRODUCTS
                    .iter()
                    .map(|product| (product.id.into(), 0))
                    .collect(),
                production_schedule: Vec::new(),
                delivery_schedule: Vec::new(),
                customer_orders: Vec::new(),
                supplier_rfqs: Vec::new(),
                production_results: Vec::new(),
                delivery_results: Vec::new(),
                last_observation_id: None,
                defaults: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut supplier_lines = Vec::new();
        for component in &COMPONENTS {
            for supplier in component.suppliers {
                let stream = format!("supplier/{supplier}/{}/initial-capacity", component.id);
                let (capacity, _) = streams.uniform_u64(&stream, 357, 743)?;
                supplier_lines.push(SupplierLine {
                    supplier_id: (*supplier).into(),
                    component_id: component.id.into(),
                    actual_capacity: u32::try_from(capacity).unwrap(),
                    inventory: 0,
                    next_ordinal: 0,
                    orders: Vec::new(),
                    offers: Vec::new(),
                    reputations: plan
                        .participants
                        .iter()
                        .map(|id| {
                            (
                                id.clone(),
                                Reputation {
                                    offered: 0,
                                    purchased: 0,
                                },
                            )
                        })
                        .collect(),
                });
            }
        }
        supplier_lines.sort_by(|a, b| {
            (&a.supplier_id, &a.component_id).cmp(&(&b.supplier_id, &b.component_id))
        });
        let capacity_market = supplier_lines
            .iter()
            .map(|line| {
                (
                    (line.supplier_id.clone(), line.component_id.clone()),
                    CapacityAccumulator::default(),
                )
            })
            .collect();
        let segments = vec![
            Segment {
                id: "low",
                q_nano: plan.initial_means_nano[0],
                trend_ppb: 1_000_000_000,
                min_nano: 25_000_000_000,
                max_nano: 100_000_000_000,
                next_ordinal: 0,
            },
            Segment {
                id: "mid",
                q_nano: plan.initial_means_nano[1],
                trend_ppb: 1_000_000_000,
                min_nano: 30_000_000_000,
                max_nano: 120_000_000_000,
                next_ordinal: 0,
            },
            Segment {
                id: "high",
                q_nano: plan.initial_means_nano[2],
                trend_ppb: 1_000_000_000,
                min_nano: 25_000_000_000,
                max_nano: 100_000_000_000,
                next_ordinal: 0,
            },
        ];
        let mut engine = Self {
            plan,
            day: 0,
            progress: Progress::Ready,
            streams,
            agents,
            supplier_lines,
            segments,
            customer_rfqs: Vec::new(),
            customer_bids: Vec::new(),
            staged_deliveries: Vec::new(),
            pending_components: vec![
                COMPONENTS
                    .iter()
                    .map(|component| (component.id.into(), 0))
                    .collect();
                6
            ],
            pending_pcs: vec![
                PRODUCTS
                    .iter()
                    .map(|product| (product.id.into(), 0))
                    .collect();
                6
            ],
            events: Vec::new(),
            event_count: 0,
            event_output,
            state_commitments: true,
            previous_event_hash: [0; 32],
            transcript: Vec::new(),
            committed_actions: Vec::new(),
            daily_commitments: Vec::new(),
            observations: Vec::new(),
            rejections: vec![Vec::new(); 6],
            product_market: PRODUCTS
                .iter()
                .map(|product| (product.id.into(), ProductAccumulator::default()))
                .collect(),
            component_market: COMPONENTS
                .iter()
                .map(|component| (component.id.into(), ComponentAccumulator::default()))
                .collect(),
            capacity_market,
            market_start_day: 0,
            current_market_report: None,
            ledger_transactions: Vec::new(),
            supplier_deliveries: vec![Vec::new(); 6],
            customer_settlements: vec![Vec::new(); 6],
            new_customer_orders: vec![Vec::new(); 6],
            daily_prices: PRODUCTS
                .iter()
                .map(|product| (product.id.into(), (None, None)))
                .collect(),
            ledger_accounts: BTreeMap::new(),
            completed: false,
            entity_ordinals: BTreeMap::new(),
            economic_ordinals: BTreeMap::new(),
            economic_kind_counts: BTreeMap::new(),
            retired_supplier_units: [0; 6],
        };
        for agent in &engine.agents {
            engine
                .ledger_accounts
                .insert(format!("agent-cash/{}", agent.id), 0);
        }
        for account in [
            "bank-interest",
            "supplier-market",
            "customer-market",
            "storage-market",
        ] {
            engine.ledger_accounts.insert(account.into(), 0);
        }
        engine.phase_open("setup")?;
        engine.emit(
            "game-declared",
            "setup",
            Value::Map(vec![
                (
                    "game_definition_id".into(),
                    Value::Text(engine.plan.game_id.clone()),
                ),
                (
                    "configuration_sha256".into(),
                    Value::Bytes(engine.plan.configuration_identity.to_vec()),
                ),
            ]),
            None,
        )?;
        engine.emit_initial_entities()?;
        engine.phase_close("setup")?;
        Ok(engine)
    }

    pub fn open_day(&mut self) -> Result<&[Observation], GameError> {
        if self.completed || self.progress != Progress::Ready {
            return Err(GameError::WrongPhase);
        }
        self.finance_open()?;
        if self.plan.historical_factors().financial_carry {
            self.historical_storage_open()?;
        }
        self.supplier_fulfillment()?;
        self.factory_execution()?;
        self.customer_settlement()?;
        self.inventory_close()?;
        self.observation_phase()?;
        self.progress = Progress::Waiting;
        Ok(&self.observations)
    }

    /// Economic command rejections produced by the most recent action commit,
    /// ordered by the fixed agent slots. The runner uses this read-only view
    /// solely to construct ActionCommitted lifecycle messages.
    #[must_use]
    pub fn latest_rejections(&self) -> &[Vec<CommandRejection>] {
        &self.rejections
    }

    pub fn commit_day(
        &mut self,
        inputs: Vec<ActionInput>,
    ) -> Result<Option<GameResult>, GameError> {
        if self.completed || self.progress != Progress::Waiting {
            return Err(GameError::WrongPhase);
        }
        let actions = self.validate_inputs(inputs)?;
        self.action_commit(actions)?;
        self.check_invariants()?;
        let commitment = self.state_commitment("action-commit");
        self.daily_commitments.push(commitment);
        if self.day == 219 {
            self.final_settlement()?;
            self.completed = true;
            self.phase_open("terminated")?;
            self.emit_state_entity_updates("terminated")?;
            let final_state = self.state_commitment("terminated");
            let mut scores: Vec<_> = self
                .agents
                .iter()
                .map(|agent| (agent.id.clone(), agent.cash))
                .collect();
            scores.sort_by(|left, right| left.0.cmp(&right.0));
            self.emit_visible(
                "game-finished",
                "terminated",
                Value::Map(vec![
                    ("status".into(), Value::Text("completed".into())),
                    (
                        "scores".into(),
                        Value::Array(
                            scores
                                .iter()
                                .map(|(agent, cash)| {
                                    Value::Map(vec![
                                        ("agent_id".into(), Value::Text(agent.clone())),
                                        ("cash_microcredits".into(), Value::I64(*cash)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
                Some(final_state),
                EventVisibility::Public,
            )?;
            self.phase_close("terminated")?;
            Ok(Some(self.result()))
        } else {
            self.day += 1;
            self.progress = Progress::Ready;
            Ok(None)
        }
    }

    fn phase_open(&mut self, phase: &'static str) -> Result<(), GameError> {
        self.entity_ordinals.clear();
        self.economic_ordinals.clear();
        self.emit(
            "phase-opened",
            phase,
            Value::Map(vec![("phase".into(), Value::Text(phase.into()))]),
            None,
        )
    }
    fn phase_close(&mut self, phase: &'static str) -> Result<(), GameError> {
        if phase != "setup" && phase != "terminated" {
            self.emit_state_entity_updates(phase)?;
        }
        let commitment = self.state_commitment(phase);
        self.emit(
            "phase-closed",
            phase,
            Value::Map(vec![("phase".into(), Value::Text(phase.into()))]),
            Some(commitment),
        )
    }

    fn emit_state_entity_updates(&mut self, phase: &'static str) -> Result<(), GameError> {
        if matches!(self.event_output, EventOutput::Off) {
            // Without event output these events only advance the event
            // count: one per agent and one per supplier line, as below.
            for _ in 0..self.agents.len() + self.supplier_lines.len() {
                self.emit_visible(
                    "entity-changed",
                    phase,
                    Value::Map(Vec::new()),
                    None,
                    EventVisibility::Operator,
                )?;
            }
            return Ok(());
        }
        let Value::Map(fields) = self.authoritative_state_value(phase) else {
            unreachable!("authoritative state is a map")
        };
        let agents = fields
            .iter()
            .find_map(|(key, value)| (key == "agents").then_some(value))
            .and_then(|value| match value {
                Value::Array(values) => Some(values.clone()),
                _ => None,
            })
            .expect("authoritative agents are an array");
        let agent_ids = self
            .agents
            .iter()
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        for (agent_id, value) in agent_ids.into_iter().zip(agents) {
            self.entity_event(
                phase,
                "agent-state",
                &agent_state_id(&agent_id),
                "updated",
                value,
                EventVisibility::Private(agent_id),
            )?;
        }

        let supplier_lines = fields
            .iter()
            .find_map(|(key, value)| (key == "supplier_lines").then_some(value))
            .and_then(|value| match value {
                Value::Array(values) => Some(values.clone()),
                _ => None,
            })
            .expect("authoritative supplier lines are an array");
        let line_ids = self
            .supplier_lines
            .iter()
            .map(|line| supplier_line_state_id(&line.supplier_id, &line.component_id))
            .collect::<Vec<_>>();
        for (line_id, value) in line_ids.into_iter().zip(supplier_lines) {
            self.entity_event(
                phase,
                "supplier-line-state",
                &line_id,
                "updated",
                value,
                EventVisibility::Operator,
            )?;
        }
        Ok(())
    }

    fn emit_operator_summary(&mut self, phase: &'static str) -> Result<(), GameError> {
        if matches!(self.event_output, EventOutput::Off) {
            return self.emit_visible(
                "operator-summary",
                phase,
                Value::Map(Vec::new()),
                None,
                EventVisibility::Operator,
            );
        }
        let agents = self
            .agents
            .iter()
            .enumerate()
            .map(|(agent_index, agent)| {
                Value::Map(vec![
                    ("agent_id".into(), Value::Text(agent.id.clone())),
                    ("cash_microcredits".into(), Value::I64(agent.cash)),
                    (
                        "component_inventory".into(),
                        Value::Array(inventory_values(&agent.id, "component", &agent.components)),
                    ),
                    (
                        "pc_inventory".into(),
                        Value::Array(inventory_values(&agent.id, "pc", &agent.pcs)),
                    ),
                    (
                        "active_customer_orders".into(),
                        Value::Array(
                            agent
                                .customer_orders
                                .iter()
                                .map(|order| customer_order_value(order, self.day))
                                .collect(),
                        ),
                    ),
                    (
                        "supplier_orders".into(),
                        Value::Array(
                            self.supplier_lines
                                .iter()
                                .flat_map(|line| {
                                    line.orders
                                        .iter()
                                        .filter(|order| order.agent_id == agent.id)
                                        .map(|order| supplier_order_value(order, &line.supplier_id))
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "production_results".into(),
                        Value::Array(
                            agent
                                .production_results
                                .iter()
                                .map(schedule_result_value)
                                .collect(),
                        ),
                    ),
                    (
                        "delivery_results".into(),
                        Value::Array(
                            agent
                                .delivery_results
                                .iter()
                                .map(schedule_result_value)
                                .collect(),
                        ),
                    ),
                    (
                        "supplier_deliveries".into(),
                        Value::Array(
                            self.supplier_deliveries[agent_index]
                                .iter()
                                .map(supplier_delivery_value)
                                .collect(),
                        ),
                    ),
                    (
                        "customer_settlements".into(),
                        Value::Array(
                            self.customer_settlements[agent_index]
                                .iter()
                                .map(customer_settlement_value)
                                .collect(),
                        ),
                    ),
                    (
                        "defaults".into(),
                        Value::U64(u64::try_from(agent.defaults.len()).unwrap()),
                    ),
                ])
            })
            .collect();
        let supplier_lines = self
            .supplier_lines
            .iter()
            .map(|line| {
                Value::Map(vec![
                    ("supplier_id".into(), Value::Text(line.supplier_id.clone())),
                    (
                        "component_id".into(),
                        Value::Text(line.component_id.clone()),
                    ),
                    (
                        "actual_capacity".into(),
                        Value::U64(u64::from(line.actual_capacity)),
                    ),
                    ("inventory".into(), Value::U64(u64::from(line.inventory))),
                ])
            })
            .collect();
        self.emit_visible(
            "operator-summary",
            phase,
            Value::Map(vec![
                ("day".into(), Value::U64(u64::from(self.day))),
                ("agents".into(), Value::Array(agents)),
                ("supplier_lines".into(), Value::Array(supplier_lines)),
            ]),
            None,
            EventVisibility::Operator,
        )
    }

    fn emit_initial_entities(&mut self) -> Result<(), GameError> {
        let mut reputations = Vec::new();
        for line_index in 0..self.supplier_lines.len() {
            let supplier_id = self.supplier_lines[line_index].supplier_id.clone();
            let component_id = self.supplier_lines[line_index].component_id.clone();
            for agent in &self.agents {
                let raw = &self.supplier_lines[line_index].reputations[&agent.id];
                reputations.push((
                    line_index,
                    supplier_id.clone(),
                    component_id.clone(),
                    SupplierReputation {
                        id: reputation_id(&supplier_id, &component_id, &agent.id),
                        supplier_id: supplier_id.clone(),
                        component_id: component_id.clone(),
                        agent_id: agent.id.clone(),
                        offered: raw.offered,
                        purchased: raw.purchased,
                        reputation_ppm: self.reputation_ppm(line_index, &agent.id),
                    },
                ));
            }
        }
        for (_, _, _, reputation) in reputations {
            self.entity_event(
                "setup",
                "supplier-reputation",
                &reputation.id,
                "created",
                supplier_reputation_value(&reputation),
                EventVisibility::Private(reputation.agent_id.clone()),
            )?;
        }
        let inventory_entities = self
            .agents
            .iter()
            .flat_map(|agent| {
                [
                    ("component", &agent.components),
                    ("pc", &agent.pcs),
                    (
                        "pending-component",
                        &self.pending_components[self.agent_index(&agent.id).unwrap()],
                    ),
                    (
                        "pending-pc",
                        &self.pending_pcs[self.agent_index(&agent.id).unwrap()],
                    ),
                ]
                .into_iter()
                .flat_map(move |(kind, items)| {
                    items.iter().map(move |(item_id, quantity)| {
                        (
                            agent.id.clone(),
                            inventory_id(&agent.id, kind, item_id),
                            inventory_item_value(&agent.id, kind, item_id, *quantity),
                        )
                    })
                })
            })
            .collect::<Vec<_>>();
        for (agent_id, entity_id, value) in inventory_entities {
            self.entity_event(
                "setup",
                "inventory-item",
                &entity_id,
                "created",
                value,
                EventVisibility::Private(agent_id),
            )?;
        }
        let state = self.authoritative_state_value("setup");
        let Value::Map(fields) = state else {
            unreachable!("authoritative state is a map")
        };
        let agent_values = fields
            .iter()
            .find_map(|(key, value)| (key == "agents").then_some(value))
            .and_then(|value| match value {
                Value::Array(values) => Some(values.clone()),
                _ => None,
            })
            .expect("authoritative agents are an array");
        for (agent, value) in self
            .agents
            .iter()
            .map(|agent| agent.id.clone())
            .zip(agent_values)
            .collect::<Vec<_>>()
        {
            self.entity_event(
                "setup",
                "agent-state",
                &agent_state_id(&agent),
                "created",
                value,
                EventVisibility::Private(agent),
            )?;
        }
        let line_values = fields
            .iter()
            .find_map(|(key, value)| (key == "supplier_lines").then_some(value))
            .and_then(|value| match value {
                Value::Array(values) => Some(values.clone()),
                _ => None,
            })
            .expect("authoritative supplier lines are an array");
        let line_ids = self
            .supplier_lines
            .iter()
            .map(|line| supplier_line_state_id(&line.supplier_id, &line.component_id))
            .collect::<Vec<_>>();
        for (line_id, value) in line_ids.into_iter().zip(line_values) {
            self.entity_event(
                "setup",
                "supplier-line-state",
                &line_id,
                "created",
                value,
                EventVisibility::Operator,
            )?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        event_type: &'static str,
        phase: &'static str,
        payload: Value,
        commitment: Option<[u8; 32]>,
    ) -> Result<(), GameError> {
        self.emit_visible(
            event_type,
            phase,
            payload,
            commitment,
            EventVisibility::Operator,
        )
    }

    fn emit_visible(
        &mut self,
        event_type: &'static str,
        phase: &'static str,
        payload: Value,
        commitment: Option<[u8; 32]>,
        visibility: EventVisibility,
    ) -> Result<(), GameError> {
        self.emit_visible_with_causes(event_type, phase, payload, commitment, visibility, &[])
    }

    fn emit_visible_with_causes(
        &mut self,
        event_type: &'static str,
        phase: &'static str,
        payload: Value,
        commitment: Option<[u8; 32]>,
        visibility: EventVisibility,
        causes: &[String],
    ) -> Result<(), GameError> {
        let index = self.event_count;
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or(GameError::Invariant("event index"))?;
        if matches!(self.event_output, EventOutput::Off) {
            return Ok(());
        }
        let mut fields = vec![
            ("schema_id".into(), Value::Text(EVENT_SCHEMA_ID.into())),
            ("event_index".into(), Value::U64(index)),
            ("event_type".into(), Value::Text(event_type.into())),
            ("game_id".into(), Value::Text(self.plan.game_id.clone())),
            ("day".into(), Value::U64(u64::from(self.day))),
            ("phase".into(), Value::Text(phase.into())),
            (
                "causes".into(),
                Value::Array(causes.iter().cloned().map(Value::Text).collect()),
            ),
            ("visibility".into(), visibility_value(&visibility)),
            ("payload".into(), payload),
            (
                "previous_event_hash".into(),
                Value::Bytes(self.previous_event_hash.to_vec()),
            ),
        ];
        if let Some(value) = commitment {
            fields.push(("state_commitment".into(), Value::Bytes(value.to_vec())));
        }
        let hash = sha256(&Value::Map(fields.clone()).encode());
        fields.push(("event_hash".into(), Value::Bytes(hash.to_vec())));
        let canonical_bytes = Value::Map(fields).encode();
        match &mut self.event_output {
            EventOutput::Off => unreachable!("event-off mode returned before encoding"),
            EventOutput::Capture => self.events.push(GameEvent {
                index,
                event_type,
                day: Some(self.day),
                phase,
                previous_hash: self.previous_event_hash,
                state_commitment: commitment,
                hash,
                canonical_bytes,
                visibility,
            }),
            EventOutput::Stream { sink, digest } => {
                digest.update(&canonical_bytes);
                sink(&canonical_bytes);
            }
        }
        self.previous_event_hash = hash;
        Ok(())
    }

    fn reject_command(
        &mut self,
        agent: usize,
        command_id: &str,
        code: &'static str,
        field_path: &str,
        detail: &'static str,
    ) -> Result<(), GameError> {
        let rejection = CommandRejection {
            command_id: command_id.into(),
            code,
            field_path: field_path.into(),
            detail,
        };
        self.rejections[agent].push(rejection.clone());
        self.economic_event(
            "command-rejection",
            "action-commit",
            None,
            Value::Map(vec![
                (
                    "agent_id".into(),
                    Value::Text(self.agents[agent].id.clone()),
                ),
                ("command_id".into(), Value::Text(command_id.into())),
                ("code".into(), Value::Text(code.into())),
                ("field_path".into(), Value::Text(field_path.into())),
                ("detail".into(), Value::Text(detail.into())),
            ]),
            EventVisibility::Private(self.agents[agent].id.clone()),
        )
    }

    fn economic_event(
        &mut self,
        kind: &'static str,
        phase: &'static str,
        subject_id: Option<&str>,
        detail: Value,
        visibility: EventVisibility,
    ) -> Result<(), GameError> {
        let ordinal = self.economic_ordinals.entry(kind).or_default();
        let economic_id = format!("{kind}/{}/{phase}/{ordinal}", self.day);
        *ordinal += 1;
        self.economic_event_with_id(kind, phase, &economic_id, subject_id, detail, visibility)
    }

    fn next_economic_id(&mut self, kind: &'static str, phase: &'static str) -> String {
        let ordinal = self.economic_ordinals.entry(kind).or_default();
        let id = format!("{kind}/{}/{phase}/{ordinal}", self.day);
        *ordinal += 1;
        id
    }

    fn next_entity_id(&mut self, entity_type: &'static str, phase: &'static str) -> String {
        let ordinal = self.entity_ordinals.entry(entity_type).or_default();
        let id = format!("{entity_type}/{}/{phase}/{ordinal}", self.day);
        *ordinal += 1;
        id
    }

    fn economic_event_with_id(
        &mut self,
        kind: &'static str,
        phase: &'static str,
        economic_id: &str,
        subject_id: Option<&str>,
        detail: Value,
        visibility: EventVisibility,
    ) -> Result<(), GameError> {
        *self.economic_kind_counts.entry(kind).or_default() += 1;
        let encoded = detail.encode();
        let mut payload = vec![
            ("economic_id".into(), Value::Text(economic_id.into())),
            ("kind".into(), Value::Text(kind.into())),
            ("value".into(), Value::Bytes(encoded.clone())),
            (
                "value_sha256".into(),
                Value::Bytes(sha256(&encoded).to_vec()),
            ),
        ];
        if let Some(subject_id) = subject_id {
            payload.push(("subject_id".into(), Value::Text(subject_id.into())));
        }
        let causes = vec![economic_id.to_owned()];
        self.emit_visible_with_causes(
            "economic-event",
            phase,
            Value::Map(payload),
            None,
            visibility,
            &causes,
        )
    }

    fn entity_event(
        &mut self,
        phase: &'static str,
        entity_type: &'static str,
        entity_id: &str,
        operation: &'static str,
        value: Value,
        visibility: EventVisibility,
    ) -> Result<(), GameError> {
        if matches!(self.event_output, EventOutput::Off) {
            // Without event output the event only advances the event count.
            return self.emit_visible(
                "entity-changed",
                phase,
                Value::Map(Vec::new()),
                None,
                visibility,
            );
        }
        let encoded = value.encode();
        self.emit_visible(
            "entity-changed",
            phase,
            Value::Map(vec![
                ("entity_type".into(), Value::Text(entity_type.into())),
                ("entity_id".into(), Value::Text(entity_id.into())),
                ("operation".into(), Value::Text(operation.into())),
                ("value".into(), Value::Bytes(encoded.clone())),
                (
                    "value_sha256".into(),
                    Value::Bytes(sha256(&encoded).to_vec()),
                ),
            ]),
            None,
            visibility,
        )
    }

    fn inventory_transition(
        &mut self,
        phase: &'static str,
        agent: usize,
        inventory_kind: &'static str,
        item_id: &str,
        before: u32,
        after: u32,
    ) -> Result<(), GameError> {
        if before == after {
            return Ok(());
        }
        let agent_id = self.agents[agent].id.clone();
        let entity_id = inventory_id(&agent_id, inventory_kind, item_id);
        self.economic_event(
            "factory-inventory",
            phase,
            Some(&entity_id),
            Value::Map(vec![
                ("agent_id".into(), Value::Text(agent_id.clone())),
                ("inventory_id".into(), Value::Text(entity_id.clone())),
                ("inventory_kind".into(), Value::Text(inventory_kind.into())),
                ("item_id".into(), Value::Text(item_id.into())),
                ("before".into(), Value::U64(u64::from(before))),
                ("after".into(), Value::U64(u64::from(after))),
            ]),
            EventVisibility::Private(agent_id.clone()),
        )?;
        self.entity_event(
            phase,
            "inventory-item",
            &entity_id,
            "updated",
            inventory_item_value(&agent_id, inventory_kind, item_id, after),
            EventVisibility::Private(agent_id),
        )
    }

    fn random_event(&mut self, phase: &'static str, draw: &RandomDraw) -> Result<(), GameError> {
        self.emit("random-draw", phase, random_draw_value(draw), None)
    }

    fn random_event_batch(
        &mut self,
        phase: &'static str,
        draws: &[RandomDraw],
    ) -> Result<(), GameError> {
        for draw in draws {
            self.random_event(phase, draw)?;
        }
        Ok(())
    }

    fn uniform_permutation(
        &mut self,
        phase: &'static str,
        decision: &str,
        context: &str,
        candidates: Vec<String>,
    ) -> Result<Vec<String>, GameError> {
        if self.plan.historical_factors().ordering_fairness
            && matches!(
                decision,
                "supplier-order-acceptance" | "supplier-rfq-allocation"
            )
        {
            return Ok(candidates);
        }
        let sampled = self.streams.uniform_permutation(
            &format!("symmetry/{}/{phase}/{decision}/{context}", self.day),
            candidates,
        )?;
        for selection in &sampled.selections {
            for draw in &selection.rejected_draws {
                self.random_event(phase, draw)?;
            }
            self.emit(
                "random-draw",
                phase,
                random_selection_value(
                    &selection.accepted_draw,
                    &selection.candidates,
                    selection.selected_index,
                    &selection.winner,
                ),
                None,
            )?;
        }
        Ok(sampled.order)
    }

    fn transfer(
        &mut self,
        phase: &'static str,
        agent: usize,
        direction: AgentTransfer<'_>,
        amount: i64,
        reason: &str,
        cause_id: &str,
    ) -> Result<(), GameError> {
        if amount <= 0 {
            return Ok(());
        }
        let (counterparty, credit_agent) = match direction {
            AgentTransfer::Credit(counterparty) => (counterparty, true),
            AgentTransfer::Debit(counterparty) => (counterparty, false),
        };
        self.agents[agent].cash = if credit_agent {
            self.agents[agent].cash.checked_add(amount)
        } else {
            self.agents[agent].cash.checked_sub(amount)
        }
        .ok_or(ArithmeticError::Overflow)?;
        let agent_account = format!("agent-cash/{}", self.agents[agent].id);
        let delta_agent = if credit_agent { amount } else { -amount };
        *self
            .ledger_accounts
            .entry(agent_account.clone())
            .or_default() += delta_agent;
        *self.ledger_accounts.entry(counterparty.into()).or_default() -= delta_agent;
        let (source_account, destination_account) = if credit_agent {
            (counterparty.to_owned(), agent_account.clone())
        } else {
            (agent_account.clone(), counterparty.to_owned())
        };
        let transaction_id = format!(
            "ledger-transaction/{}",
            hex(&sha256(
                &Value::Array(vec![
                    Value::U64(u64::from(self.day)),
                    Value::Text(phase.into()),
                    Value::Text(self.agents[agent].id.clone()),
                    Value::Text(reason.into()),
                    Value::Text(cause_id.into()),
                ])
                .encode()
            ))
        );
        let transaction = LedgerTransaction {
            id: transaction_id,
            day: self.day,
            phase,
            source_account,
            destination_account,
            amount,
            reason: reason.into(),
            cause_id: cause_id.into(),
        };
        let transaction_value = ledger_transaction_value(&transaction);
        self.ledger_transactions.push(transaction);
        self.emit_visible_with_causes(
            "ledger-transferred",
            phase,
            Value::Map(vec![("transaction".into(), transaction_value)]),
            None,
            EventVisibility::Private(self.agents[agent].id.clone()),
            &[cause_id.into()],
        )
    }

    fn finance_open(&mut self) -> Result<(), GameError> {
        self.phase_open("finance-open")?;
        let historical = self.plan.historical_factors().financial_carry;
        for index in 0..self.agents.len() {
            let cash = self.agents[index].cash;
            let rate = if cash < 0 {
                self.plan.loan_rate_ppb
            } else {
                self.plan.deposit_rate_ppb
            };
            let (amount, credit_agent) = if historical {
                let annual_percent = rate / 10_000_000;
                let target = historical_finance_balance(cash, annual_percent)?;
                let delta = target.checked_sub(cash).ok_or(ArithmeticError::Overflow)?;
                (
                    delta.checked_abs().ok_or(ArithmeticError::Overflow)?,
                    delta > 0,
                )
            } else {
                (daily_interest(cash, rate)?, cash >= 0)
            };
            let cause_id = format!("finance-assessment/{}/{}", self.day, self.agents[index].id);
            if amount > 0 {
                self.economic_event_with_id(
                    "bank-interest-assessment",
                    "finance-open",
                    &cause_id,
                    Some(&agent_state_id(&self.agents[index].id)),
                    Value::Map(vec![
                        (
                            "agent_id".into(),
                            Value::Text(self.agents[index].id.clone()),
                        ),
                        ("opening_cash".into(), Value::I64(cash)),
                        ("annual_rate_ppb".into(), Value::U64(u64::from(rate))),
                        ("amount".into(), Value::I64(amount)),
                        (
                            "direction".into(),
                            Value::Text(if credit_agent { "credit" } else { "debit" }.into()),
                        ),
                    ]),
                    EventVisibility::Private(self.agents[index].id.clone()),
                )?;
            }
            if amount > 0 && !credit_agent {
                self.transfer(
                    "finance-open",
                    index,
                    AgentTransfer::Debit("bank-interest"),
                    amount,
                    "bank-interest",
                    &cause_id,
                )?;
            } else if amount > 0 && credit_agent {
                self.transfer(
                    "finance-open",
                    index,
                    AgentTransfer::Credit("bank-interest"),
                    amount,
                    "bank-interest",
                    &cause_id,
                )?;
            }
        }
        self.phase_close("finance-open")
    }

    fn historical_storage_open(&mut self) -> Result<(), GameError> {
        self.phase_open("historical-storage-open")?;
        let annual_percent = self.plan.storage_rate_ppb / 10_000_000;
        for agent in 0..self.agents.len() {
            let inventory_value = self.inventory_value_microcredits(agent)?;
            let charge = historical_storage_charge(
                u128::try_from(inventory_value).map_err(|_| ArithmeticError::Overflow)?,
                annual_percent,
            )?;
            let cause_id = format!("storage-assessment/{}/{}", self.day, self.agents[agent].id);
            if charge > 0 {
                self.economic_event_with_id(
                    "storage-assessment",
                    "historical-storage-open",
                    &cause_id,
                    Some(&agent_state_id(&self.agents[agent].id)),
                    Value::Map(vec![
                        (
                            "agent_id".into(),
                            Value::Text(self.agents[agent].id.clone()),
                        ),
                        (
                            "inventory_value".into(),
                            Value::I64(
                                i64::try_from(inventory_value)
                                    .map_err(|_| ArithmeticError::Overflow)?,
                            ),
                        ),
                        (
                            "annual_rate_ppb".into(),
                            Value::U64(u64::from(self.plan.storage_rate_ppb)),
                        ),
                        ("amount".into(), Value::I64(charge)),
                    ]),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
            }
            self.transfer(
                "historical-storage-open",
                agent,
                AgentTransfer::Debit("storage-market"),
                charge,
                "storage-charge",
                &cause_id,
            )?;
        }
        self.phase_close("historical-storage-open")
    }

    fn supplier_fulfillment(&mut self) -> Result<(), GameError> {
        self.phase_open("supplier-fulfillment")?;
        for line_index in 0..self.supplier_lines.len() {
            if self.day > 0 {
                let name = format!(
                    "supplier/{}/{}/capacity/{}",
                    self.supplier_lines[line_index].supplier_id,
                    self.supplier_lines[line_index].component_id,
                    self.day
                );
                let draw = self.streams.draw_once_u64(&name)?;
                self.random_event("supplier-fulfillment", &draw)?;
                self.supplier_lines[line_index].actual_capacity =
                    supplier_capacity(self.supplier_lines[line_index].actual_capacity, draw.raw)?;
                let line_id = supplier_line_state_id(
                    &self.supplier_lines[line_index].supplier_id,
                    &self.supplier_lines[line_index].component_id,
                );
                self.economic_event(
                    "supplier-production",
                    "supplier-fulfillment",
                    Some(&line_id),
                    Value::Map(vec![(
                        "actual_capacity".into(),
                        Value::U64(u64::from(self.supplier_lines[line_index].actual_capacity)),
                    )]),
                    EventVisibility::Operator,
                )?;
            }
            let component_id = self.supplier_lines[line_index].component_id.clone();
            let supplier_id = self.supplier_lines[line_index].supplier_id.clone();
            let capacity_market = self
                .capacity_market
                .get_mut(&(supplier_id, component_id.clone()))
                .expect("supplier line has a capacity accumulator");
            capacity_market.capacity_sum = capacity_market
                .capacity_sum
                .checked_add(u64::from(self.supplier_lines[line_index].actual_capacity))
                .ok_or(ArithmeticError::Overflow)?;
            capacity_market.capacity_count += 1;
            let outstanding: u32 = self.supplier_lines[line_index]
                .orders
                .iter()
                .map(|order| order.quantity - order.delivered)
                .sum();
            if outstanding > self.supplier_lines[line_index].inventory {
                let before = self.supplier_lines[line_index].inventory;
                self.supplier_lines[line_index].inventory =
                    self.supplier_lines[line_index].inventory.saturating_add(
                        self.supplier_lines[line_index]
                            .actual_capacity
                            .min(outstanding - self.supplier_lines[line_index].inventory),
                    );
                if before != self.supplier_lines[line_index].inventory {
                    let line_id = supplier_line_state_id(
                        &self.supplier_lines[line_index].supplier_id,
                        &self.supplier_lines[line_index].component_id,
                    );
                    self.economic_event(
                        "supplier-inventory",
                        "supplier-fulfillment",
                        Some(&line_id),
                        Value::Map(vec![
                            ("before".into(), Value::U64(u64::from(before))),
                            (
                                "after".into(),
                                Value::U64(u64::from(self.supplier_lines[line_index].inventory)),
                            ),
                        ]),
                        EventVisibility::Operator,
                    )?;
                }
            }
            self.supplier_lines[line_index]
                .orders
                .sort_by_key(|order| (order.due_day, order.ordinal));
            let mut deliveries = Vec::new();
            for order_index in 0..self.supplier_lines[line_index].orders.len() {
                let order = &self.supplier_lines[line_index].orders[order_index];
                if order.due_day > self.day || order.delivered == order.quantity {
                    continue;
                }
                let remaining = order.quantity - order.delivered;
                if self.supplier_lines[line_index].inventory < remaining {
                    break;
                }
                deliveries.push((order_index, remaining));
                self.supplier_lines[line_index].inventory -= remaining;
            }
            for (order_index, quantity) in deliveries {
                let (agent_id, component, unit_price, down, order_id) = {
                    let order = &mut self.supplier_lines[line_index].orders[order_index];
                    order.delivered += quantity;
                    (
                        order.agent_id.clone(),
                        order.component_id.clone(),
                        order.unit_price,
                        order.down_payment,
                        order.id.clone(),
                    )
                };
                let agent = self.agent_index(&agent_id)?;
                self.component_market
                    .get_mut(&component)
                    .unwrap()
                    .delivered_quantity += u64::from(quantity);
                let pending_before = self.pending_components[agent][&component];
                let pending_after = pending_before
                    .checked_add(quantity)
                    .ok_or(ArithmeticError::Overflow)?;
                self.pending_components[agent].insert(component.clone(), pending_after);
                self.inventory_transition(
                    "supplier-fulfillment",
                    agent,
                    "pending-component",
                    &component,
                    pending_before,
                    pending_after,
                )?;
                let total =
                    i64::try_from(i128::from(quantity) * i128::from(unit_price) - i128::from(down))
                        .map_err(|_| ArithmeticError::Overflow)?;
                let delivery_id = self.next_entity_id("supplier-delivery", "supplier-fulfillment");
                let delivery = SupplierDelivery {
                    id: delivery_id.clone(),
                    order_id: order_id.clone(),
                    component_id: component.clone(),
                    quantity,
                    unit_price,
                    invoice: total,
                };
                self.supplier_deliveries[agent].push(delivery.clone());
                let updated_order = self.supplier_lines[line_index].orders[order_index].clone();
                self.entity_event(
                    "supplier-fulfillment",
                    "supplier-order",
                    &updated_order.id,
                    "updated",
                    supplier_order_value(
                        &updated_order,
                        &self.supplier_lines[line_index].supplier_id,
                    ),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
                self.entity_event(
                    "supplier-fulfillment",
                    "supplier-delivery",
                    &delivery_id,
                    "created",
                    supplier_delivery_value(&delivery),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
                self.transfer(
                    "supplier-fulfillment",
                    agent,
                    AgentTransfer::Debit("supplier-market"),
                    total,
                    "supplier-delivery-balance",
                    &delivery_id,
                )?;
            }
        }
        self.phase_close("supplier-fulfillment")
    }

    fn factory_execution(&mut self) -> Result<(), GameError> {
        self.phase_open("factory-execution")?;
        self.staged_deliveries.clear();
        let mut schedule_result_ordinal = 0_u64;
        for agent in 0..6 {
            let schedule_agent_id = self.agents[agent].id.clone();
            self.agents[agent].delivery_results.clear();
            self.agents[agent].production_results.clear();
            self.agents[agent]
                .delivery_schedule
                .sort_by(|a, b| (a.priority, &a.line_id).cmp(&(b.priority, &b.line_id)));
            let deliveries = self.agents[agent].delivery_schedule.clone();
            for line in deliveries {
                let result_id = schedule_result_id(self.day, schedule_result_ordinal);
                schedule_result_ordinal += 1;
                let order_index = self.agents[agent].customer_orders.iter().position(|order| {
                    order.id == line.customer_order_id
                        && order.status == CustomerOrderStatus::Open
                        && order.quantity == line.quantity
                });
                if let Some(order_index) = order_index {
                    let product = self.agents[agent].customer_orders[order_index]
                        .product_id
                        .clone();
                    let available = *self.agents[agent].pcs.get(&product).unwrap_or(&0);
                    if available >= line.quantity {
                        let after = available - line.quantity;
                        self.agents[agent].pcs.insert(product.clone(), after);
                        self.inventory_transition(
                            "factory-execution",
                            agent,
                            "pc",
                            &product,
                            available,
                            after,
                        )?;
                        self.staged_deliveries
                            .push((agent, order_index, line.clone()));
                        self.agents[agent].delivery_results.push(ScheduleResult {
                            id: result_id.clone(),
                            agent_id: schedule_agent_id.clone(),
                            schedule_kind: "delivery",
                            line_id: line.line_id.clone(),
                            executed: true,
                            quantity: line.quantity,
                            rejection: None,
                        });
                        self.economic_event(
                            "factory-delivery",
                            "factory-execution",
                            None,
                            Value::Map(vec![
                                (
                                    "agent_id".into(),
                                    Value::Text(self.agents[agent].id.clone()),
                                ),
                                ("line_id".into(), Value::Text(line.line_id.clone())),
                                ("result_id".into(), Value::Text(result_id)),
                                (
                                    "order_id".into(),
                                    Value::Text(line.customer_order_id.clone()),
                                ),
                                ("product_id".into(), Value::Text(product)),
                                ("quantity".into(), Value::U64(u64::from(line.quantity))),
                            ]),
                            EventVisibility::Private(self.agents[agent].id.clone()),
                        )?;
                        continue;
                    }
                }
                let (code, detail) = if order_index.is_none() {
                    (
                        "unknown-reference",
                        "customer order is unavailable or quantity mismatches",
                    )
                } else {
                    ("insufficient-inventory", "PC inventory is insufficient")
                };
                let rejection = CommandRejection {
                    command_id: line.line_id.clone(),
                    code,
                    field_path: "delivery_schedule".into(),
                    detail,
                };
                self.agents[agent].delivery_results.push(ScheduleResult {
                    id: result_id,
                    agent_id: schedule_agent_id.clone(),
                    schedule_kind: "delivery",
                    line_id: line.line_id.clone(),
                    executed: false,
                    quantity: 0,
                    rejection: Some(rejection),
                });
                self.economic_event(
                    "command-rejection",
                    "factory-execution",
                    None,
                    Value::Map(vec![
                        ("agent_id".into(), Value::Text(schedule_agent_id.clone())),
                        ("command_id".into(), Value::Text(line.line_id.clone())),
                        ("code".into(), Value::Text(code.into())),
                        ("field_path".into(), Value::Text("delivery_schedule".into())),
                        ("detail".into(), Value::Text(detail.into())),
                    ]),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
            }
            self.agents[agent]
                .production_schedule
                .sort_by(|a, b| (a.priority, &a.line_id).cmp(&(b.priority, &b.line_id)));
            let lines = self.agents[agent].production_schedule.clone();
            let mut cycles = FACTORY_CYCLES;
            for line in lines {
                let result_id = schedule_result_id(self.day, schedule_result_ordinal);
                schedule_result_ordinal += 1;
                let Some(product) = PRODUCTS
                    .iter()
                    .find(|product| product.id == line.product_id)
                else {
                    let rejection = CommandRejection {
                        command_id: line.line_id.clone(),
                        code: "unknown-reference",
                        field_path: "production_schedule.product_id".into(),
                        detail: "product does not exist",
                    };
                    self.agents[agent].production_results.push(ScheduleResult {
                        id: result_id,
                        agent_id: schedule_agent_id.clone(),
                        schedule_kind: "production",
                        line_id: line.line_id.clone(),
                        executed: false,
                        quantity: 0,
                        rejection: Some(rejection),
                    });
                    continue;
                };
                let required_cycles = product
                    .cycles
                    .checked_mul(line.quantity)
                    .ok_or(ArithmeticError::Overflow)?;
                if required_cycles <= cycles
                    && product.components.iter().all(|component| {
                        self.agents[agent]
                            .components
                            .get(*component)
                            .copied()
                            .unwrap_or(0)
                            >= line.quantity
                    })
                {
                    let mut component_transitions = Vec::new();
                    for component in product.components {
                        let before = self.agents[agent].components[component];
                        let after = before - line.quantity;
                        self.agents[agent]
                            .components
                            .insert((*component).into(), after);
                        component_transitions.push((component, before, after));
                    }
                    cycles -= required_cycles;
                    let pending_before = self.pending_pcs[agent][product.id];
                    let pending_after = pending_before
                        .checked_add(line.quantity)
                        .ok_or(ArithmeticError::Overflow)?;
                    self.pending_pcs[agent].insert(product.id.into(), pending_after);
                    for (component, before, after) in component_transitions {
                        self.inventory_transition(
                            "factory-execution",
                            agent,
                            "component",
                            component,
                            before,
                            after,
                        )?;
                    }
                    self.inventory_transition(
                        "factory-execution",
                        agent,
                        "pending-pc",
                        product.id,
                        pending_before,
                        pending_after,
                    )?;
                    self.agents[agent].production_results.push(ScheduleResult {
                        id: result_id.clone(),
                        agent_id: schedule_agent_id.clone(),
                        schedule_kind: "production",
                        line_id: line.line_id.clone(),
                        executed: true,
                        quantity: line.quantity,
                        rejection: None,
                    });
                    self.economic_event(
                        "factory-production",
                        "factory-execution",
                        None,
                        Value::Map(vec![
                            (
                                "agent_id".into(),
                                Value::Text(self.agents[agent].id.clone()),
                            ),
                            ("line_id".into(), Value::Text(line.line_id.clone())),
                            ("result_id".into(), Value::Text(result_id)),
                            ("product_id".into(), Value::Text(line.product_id.clone())),
                            ("quantity".into(), Value::U64(u64::from(line.quantity))),
                            ("cycles".into(), Value::U64(u64::from(required_cycles))),
                        ]),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                } else {
                    let code = if required_cycles > cycles {
                        "insufficient-capacity"
                    } else {
                        "insufficient-inventory"
                    };
                    let rejection = CommandRejection {
                        command_id: line.line_id.clone(),
                        code,
                        field_path: "production_schedule".into(),
                        detail: "line is atomic and cannot execute with available resources",
                    };
                    self.agents[agent].production_results.push(ScheduleResult {
                        id: result_id,
                        agent_id: schedule_agent_id.clone(),
                        schedule_kind: "production",
                        line_id: line.line_id.clone(),
                        executed: false,
                        quantity: 0,
                        rejection: Some(rejection),
                    });
                    self.economic_event(
                        "command-rejection",
                        "factory-execution",
                        None,
                        Value::Map(vec![
                            ("agent_id".into(), Value::Text(schedule_agent_id.clone())),
                            ("command_id".into(), Value::Text(line.line_id.clone())),
                            ("code".into(), Value::Text(code.into())),
                            (
                                "field_path".into(),
                                Value::Text("production_schedule".into()),
                            ),
                            (
                                "detail".into(),
                                Value::Text(
                                    "line is atomic and cannot execute with available resources"
                                        .into(),
                                ),
                            ),
                        ]),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                }
            }
            let schedule_results = self.agents[agent]
                .delivery_results
                .iter()
                .chain(self.agents[agent].production_results.iter())
                .cloned()
                .collect::<Vec<_>>();
            for result in schedule_results {
                self.entity_event(
                    "factory-execution",
                    "schedule-result",
                    &result.id,
                    "created",
                    schedule_result_value(&result),
                    EventVisibility::Private(schedule_agent_id.clone()),
                )?;
            }
            self.agents[agent].delivery_schedule.clear();
            self.agents[agent].production_schedule.clear();
        }
        self.phase_close("factory-execution")
    }

    fn customer_settlement(&mut self) -> Result<(), GameError> {
        self.phase_open("customer-settlement")?;
        for agent in 0..6 {
            for order_index in 0..self.agents[agent].customer_orders.len() {
                let status = self.agents[agent].customer_orders[order_index]
                    .status
                    .clone();
                let due = self.agents[agent].customer_orders[order_index].due_day;
                if status == CustomerOrderStatus::Open && self.day > due {
                    let penalty = self.agents[agent].customer_orders[order_index].penalty;
                    self.agents[agent].customer_orders[order_index].penalty_count += 1;
                    let order_id = self.agents[agent].customer_orders[order_index].id.clone();
                    let penalty_count =
                        self.agents[agent].customer_orders[order_index].penalty_count;
                    let cancelled = penalty_count == 5;
                    if cancelled {
                        self.agents[agent].customer_orders[order_index].status =
                            CustomerOrderStatus::Cancelled;
                    }
                    let cause_id = format!("customer-penalty/{order_id}/{penalty_count}");
                    let settlement_id =
                        self.next_entity_id("customer-settlement", "customer-settlement");
                    let settlement = CustomerSettlement {
                        id: settlement_id.clone(),
                        order_id: order_id.clone(),
                        delivered: false,
                        payment: 0,
                        penalty,
                        penalty_count,
                        cancelled,
                    };
                    self.customer_settlements[agent].push(settlement.clone());
                    self.entity_event(
                        "customer-settlement",
                        "customer-order",
                        &order_id,
                        "updated",
                        customer_order_value(
                            &self.agents[agent].customer_orders[order_index],
                            self.day,
                        ),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.entity_event(
                        "customer-settlement",
                        "customer-settlement",
                        &settlement_id,
                        "created",
                        customer_settlement_value(&settlement),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.economic_event_with_id(
                        "customer-penalty",
                        "customer-settlement",
                        &cause_id,
                        Some(&order_id),
                        Value::Map(vec![
                            ("settlement_id".into(), Value::Text(settlement_id.clone())),
                            ("penalty_count".into(), Value::U64(u64::from(penalty_count))),
                            ("amount".into(), Value::I64(penalty)),
                        ]),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.transfer(
                        "customer-settlement",
                        agent,
                        AgentTransfer::Debit("customer-market"),
                        penalty,
                        "late-penalty",
                        &cause_id,
                    )?;
                    if cancelled {
                        self.economic_event(
                            "customer-cancellation",
                            "customer-settlement",
                            Some(&order_id),
                            Value::Map(vec![
                                ("settlement_id".into(), Value::Text(settlement_id)),
                                ("penalty_count".into(), Value::U64(5)),
                            ]),
                            EventVisibility::Private(self.agents[agent].id.clone()),
                        )?;
                    }
                }
            }
        }
        for (agent, order_index, _) in self.staged_deliveries.clone() {
            if self.agents[agent].customer_orders[order_index].status
                == CustomerOrderStatus::Cancelled
            {
                let product = self.agents[agent].customer_orders[order_index]
                    .product_id
                    .clone();
                let quantity = self.agents[agent].customer_orders[order_index].quantity;
                let before = self.agents[agent].pcs[&product];
                let after = before
                    .checked_add(quantity)
                    .ok_or(ArithmeticError::Overflow)?;
                self.agents[agent].pcs.insert(product.clone(), after);
                self.inventory_transition(
                    "customer-settlement",
                    agent,
                    "pc",
                    &product,
                    before,
                    after,
                )?;
            } else {
                let order_id = self.agents[agent].customer_orders[order_index].id.clone();
                self.agents[agent].customer_orders[order_index].status =
                    CustomerOrderStatus::DeliveredPending {
                        delivery_day: self.day,
                    };
                self.agents[agent].customer_orders[order_index].delivery_day = Some(self.day);
                let settlement = CustomerSettlement {
                    id: self.next_entity_id("customer-settlement", "customer-settlement"),
                    order_id: order_id.clone(),
                    delivered: true,
                    payment: 0,
                    penalty: 0,
                    penalty_count: self.agents[agent].customer_orders[order_index].penalty_count,
                    cancelled: false,
                };
                self.customer_settlements[agent].push(settlement.clone());
                self.entity_event(
                    "customer-settlement",
                    "customer-order",
                    &order_id,
                    "updated",
                    customer_order_value(
                        &self.agents[agent].customer_orders[order_index],
                        self.day,
                    ),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
                self.entity_event(
                    "customer-settlement",
                    "customer-settlement",
                    &settlement.id,
                    "created",
                    customer_settlement_value(&settlement),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
            }
        }
        for agent in 0..6 {
            for order_index in 0..self.agents[agent].customer_orders.len() {
                if let CustomerOrderStatus::DeliveredPending { delivery_day } =
                    self.agents[agent].customer_orders[order_index].status
                    && self.day >= self.agents[agent].customer_orders[order_index].due_day
                    && self.day >= delivery_day
                {
                    let amount = i64::try_from(
                        i128::from(self.agents[agent].customer_orders[order_index].quantity)
                            * i128::from(
                                self.agents[agent].customer_orders[order_index].unit_price,
                            ),
                    )
                    .map_err(|_| ArithmeticError::Overflow)?;
                    let order_id = self.agents[agent].customer_orders[order_index].id.clone();
                    self.agents[agent].customer_orders[order_index].status =
                        CustomerOrderStatus::Delivered;
                    let settlement = CustomerSettlement {
                        id: self.next_entity_id("customer-settlement", "customer-settlement"),
                        order_id: order_id.clone(),
                        delivered: true,
                        payment: amount,
                        penalty: 0,
                        penalty_count: self.agents[agent].customer_orders[order_index]
                            .penalty_count,
                        cancelled: false,
                    };
                    self.customer_settlements[agent].push(settlement.clone());
                    self.entity_event(
                        "customer-settlement",
                        "customer-order",
                        &order_id,
                        "updated",
                        customer_order_value(
                            &self.agents[agent].customer_orders[order_index],
                            self.day,
                        ),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.entity_event(
                        "customer-settlement",
                        "customer-settlement",
                        &settlement.id,
                        "created",
                        customer_settlement_value(&settlement),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    let payment_id =
                        self.next_economic_id("customer-payment", "customer-settlement");
                    self.economic_event_with_id(
                        "customer-payment",
                        "customer-settlement",
                        &payment_id,
                        Some(&order_id),
                        Value::Map(vec![
                            ("settlement_id".into(), Value::Text(settlement.id.clone())),
                            ("amount".into(), Value::I64(amount)),
                        ]),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.transfer(
                        "customer-settlement",
                        agent,
                        AgentTransfer::Credit("customer-market"),
                        amount,
                        "customer-payment",
                        &payment_id,
                    )?;
                }
            }
        }
        self.award_prior_bids()?;
        self.generate_customer_rfqs()?;
        if (self.day + 1).is_multiple_of(20) {
            self.publish_market_report()?;
        }
        self.phase_close("customer-settlement")
    }

    fn award_prior_bids(&mut self) -> Result<(), GameError> {
        let mut rfq_ids: Vec<String> = self
            .customer_bids
            .iter()
            .filter(|(award_day, _, _)| *award_day == self.day)
            .map(|(_, _, offer)| offer.rfq_id.clone())
            .collect();
        rfq_ids.sort();
        rfq_ids.dedup();
        let mut awarded_rfqs = Vec::new();
        for rfq_id in rfq_ids {
            let Some(rfq) = self
                .customer_rfqs
                .iter()
                .find(|rfq| rfq.id == rfq_id)
                .cloned()
            else {
                continue;
            };
            let mut bids: Vec<(String, String, i64)> = self
                .customer_bids
                .iter()
                .filter(|(award_day, _, offer)| {
                    *award_day == self.day
                        && offer.rfq_id == rfq_id
                        && offer.unit_price >= 0
                        && offer.unit_price <= rfq.reserve
                })
                .map(|(award_day, agent, offer)| {
                    (
                        customer_bid_id(*award_day, agent, offer),
                        agent.clone(),
                        offer.unit_price,
                    )
                })
                .collect();
            if bids.is_empty() {
                continue;
            }
            bids.sort();
            let lowest = bids.iter().map(|(_, _, price)| *price).min().unwrap();
            let tied: Vec<_> = bids
                .into_iter()
                .filter(|(_, _, price)| *price == lowest)
                .collect();
            let winning_bid_id = if tied.len() == 1 {
                tied[0].0.clone()
            } else {
                self.uniform_permutation(
                    "customer-settlement",
                    "customer-award",
                    &rfq_id,
                    tied.iter().map(|(bid_id, _, _)| bid_id.clone()).collect(),
                )?[0]
                    .clone()
            };
            let winner = tied
                .iter()
                .find(|(bid_id, _, _)| *bid_id == winning_bid_id)
                .map(|(_, agent, _)| agent.clone())
                .expect("winning bid belongs to a tied agent");
            let agent = self.agent_index(&winner)?;
            let order = CustomerOrder {
                id: format!("customer-order/{rfq_id}"),
                rfq_id: rfq_id.clone(),
                agent_id: winner,
                product_id: rfq.product_id,
                quantity: rfq.quantity,
                due_day: rfq.due_day,
                unit_price: lowest,
                penalty: rfq.penalty,
                penalty_count: 0,
                status: CustomerOrderStatus::Open,
                delivery_day: None,
            };
            let market = self.product_market.get_mut(&order.product_id).unwrap();
            market.order_count += 1;
            market.order_quantity += u64::from(order.quantity);
            market.winning_price_sum += i128::from(order.quantity) * i128::from(order.unit_price);
            market.winning_price_count += u64::from(order.quantity);
            market.winning_min = Some(
                market
                    .winning_min
                    .map_or(order.unit_price, |value| value.min(order.unit_price)),
            );
            market.winning_max = Some(
                market
                    .winning_max
                    .map_or(order.unit_price, |value| value.max(order.unit_price)),
            );
            let range = self.daily_prices.get_mut(&order.product_id).unwrap();
            range.0 = Some(
                range
                    .0
                    .map_or(order.unit_price, |value| value.min(order.unit_price)),
            );
            range.1 = Some(
                range
                    .1
                    .map_or(order.unit_price, |value| value.max(order.unit_price)),
            );
            self.entity_event(
                "customer-settlement",
                "customer-order",
                &order.id,
                "created",
                customer_order_value(&order, self.day),
                EventVisibility::Private(self.agents[agent].id.clone()),
            )?;
            self.economic_event(
                "customer-award",
                "customer-settlement",
                Some(&order.id),
                Value::Map(vec![
                    ("bid_id".into(), Value::Text(winning_bid_id)),
                    ("order_id".into(), Value::Text(order.id.clone())),
                ]),
                EventVisibility::Private(self.agents[agent].id.clone()),
            )?;
            self.new_customer_orders[agent].push(order.clone());
            self.agents[agent].customer_orders.push(order);
            awarded_rfqs.push(rfq_id);
        }
        let expired_bids = self
            .customer_bids
            .iter()
            .filter(|(award_day, _, _)| *award_day <= self.day)
            .cloned()
            .collect::<Vec<_>>();
        for (award_day, agent_id, offer) in expired_bids {
            self.entity_event(
                "customer-settlement",
                "customer-bid",
                &customer_bid_id(award_day, &agent_id, &offer),
                "removed",
                customer_bid_value(award_day, &agent_id, &offer),
                EventVisibility::Private(agent_id),
            )?;
        }
        self.customer_bids
            .retain(|(award_day, _, _)| *award_day > self.day);
        // Complete the bid lifecycles before removing their awarded RFQs.
        // Unawarded RFQs remain eligible until their existing expiry.
        for rfq_id in awarded_rfqs {
            let index = self
                .customer_rfqs
                .iter()
                .position(|rfq| rfq.id == rfq_id)
                .expect("awarded RFQ remains present until settlement cleanup");
            let rfq = self.customer_rfqs.remove(index);
            self.entity_event(
                "customer-settlement",
                "customer-rfq",
                &rfq.id,
                "removed",
                customer_rfq_value(&rfq),
                EventVisibility::Public,
            )?;
        }
        Ok(())
    }

    fn generate_customer_rfqs(&mut self) -> Result<(), GameError> {
        let rules = self.plan.rules();
        // Draft 3: every RFQ still present was issued on an earlier day, and an
        // RFQ takes offers only on its issue day, so it closes now.
        let expired_rfqs = self
            .customer_rfqs
            .iter()
            .filter(|rfq| rfq.due_day < self.day || rules.issue_day_customer_bids)
            .cloned()
            .collect::<Vec<_>>();
        for rfq in expired_rfqs {
            self.entity_event(
                "customer-settlement",
                "customer-rfq",
                &rfq.id,
                "removed",
                customer_rfq_value(&rfq),
                EventVisibility::Public,
            )?;
        }
        self.customer_rfqs
            .retain(|rfq| rfq.due_day >= self.day && !rules.issue_day_customer_bids);
        // Draft 3: customers issue no RFQs on day 0. The day-0 count and
        // attribute streams are named per day and RFQ, so leaving them unread
        // shifts no later draw; the demand walk below still takes its day-0
        // step, so every later day sees the same demand as under draft 2.
        let issue_rfqs = !(rules.no_day_zero_customer_rfqs && self.day == 0);
        for segment_index in 0..3 {
            let segment_id = self.segments[segment_index].id;
            let count = if issue_rfqs {
                let count_stream = format!("customer/{segment_id}/count/{}", self.day);
                let (count, draw) = self
                    .streams
                    .poisson(&count_stream, self.segments[segment_index].q_nano)?;
                self.segments[segment_index].next_ordinal = u64::from(count);
                self.random_event("customer-settlement", &draw)?;
                count
            } else {
                self.segments[segment_index].next_ordinal = 0;
                0
            };
            let products: Vec<_> = PRODUCTS
                .iter()
                .filter(|product| product.segment == segment_id)
                .collect();
            for ordinal in 0..count {
                let id = format!("customer-rfq/{}/{segment_id}/{ordinal}", self.day);
                let mut attribute_draws = Vec::new();
                let (product_index, draws) = self.streams.uniform_u64(
                    &format!("customer/{id}/product"),
                    0,
                    u64::try_from(products.len() - 1).unwrap(),
                )?;
                attribute_draws.extend(draws);
                let (quantity, draws) =
                    self.streams
                        .uniform_u64(&format!("customer/{id}/quantity"), 1, 20)?;
                attribute_draws.extend(draws);
                let (lead, draws) =
                    self.streams
                        .uniform_u64(&format!("customer/{id}/lead-time"), 3, 12)?;
                attribute_draws.extend(draws);
                let (reserve_percent, draws) =
                    self.streams
                        .uniform_u64(&format!("customer/{id}/reserve"), 75, 125)?;
                attribute_draws.extend(draws);
                let (penalty_percent, draws) =
                    self.streams
                        .uniform_u64(&format!("customer/{id}/penalty"), 5, 15)?;
                attribute_draws.extend(draws);
                self.random_event_batch("customer-settlement", &attribute_draws)?;
                let product = products[usize::try_from(product_index).unwrap()];
                let nominal: i64 = product
                    .components
                    .iter()
                    .map(|id| {
                        COMPONENTS
                            .iter()
                            .find(|component| component.id == *id)
                            .unwrap()
                            .base_microcredits
                    })
                    .sum();
                let reserve =
                    i64::try_from(i128::from(nominal) * i128::from(reserve_percent) / 100)
                        .map_err(|_| ArithmeticError::Overflow)?;
                let penalty = i64::try_from(
                    i128::from(quantity) * i128::from(reserve) * i128::from(penalty_percent) / 100,
                )
                .map_err(|_| ArithmeticError::Overflow)?;
                let due_day = self.day + u32::try_from(lead).unwrap();
                if due_day <= 219 {
                    let market = self.product_market.get_mut(product.id).unwrap();
                    market.rfq_count += 1;
                    market.rfq_quantity += quantity;
                    let rfq = CustomerRfq {
                        id,
                        product_id: product.id.into(),
                        quantity: u32::try_from(quantity).unwrap(),
                        due_day,
                        reserve,
                        penalty,
                    };
                    self.entity_event(
                        "customer-settlement",
                        "customer-rfq",
                        &rfq.id,
                        "created",
                        customer_rfq_value(&rfq),
                        EventVisibility::Public,
                    )?;
                    self.customer_rfqs.push(rfq);
                }
            }
            let trend_stream = format!("customer/{segment_id}/trend/{}", self.day);
            let (epsilon, draws) =
                self.streams
                    .uniform_i64(&trend_stream, -10_000_000, 10_000_000)?;
            for draw in &draws {
                self.random_event("customer-settlement", draw)?;
            }
            let (q, trend) = customer_trend(
                self.segments[segment_index].q_nano,
                self.segments[segment_index].trend_ppb,
                epsilon,
                self.segments[segment_index].min_nano,
                self.segments[segment_index].max_nano,
            )?;
            self.segments[segment_index].q_nano = q;
            self.segments[segment_index].trend_ppb = trend;
            self.economic_event(
                "customer-demand",
                "customer-settlement",
                None,
                segment_state_value(&self.segments[segment_index]),
                EventVisibility::Operator,
            )?;
        }
        self.customer_rfqs.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(())
    }

    fn publish_market_report(&mut self) -> Result<(), GameError> {
        let products = PRODUCTS
            .iter()
            .map(|spec| {
                let accumulator = &self.product_market[spec.id];
                let mean = if accumulator.winning_price_count == 0 {
                    None
                } else {
                    Some(
                        i64::try_from(crate::arithmetic::round_half_even(
                            u128::try_from(accumulator.winning_price_sum)
                                .map_err(|_| ArithmeticError::Overflow)?,
                            u128::from(accumulator.winning_price_count),
                        )?)
                        .map_err(|_| ArithmeticError::Overflow)?,
                    )
                };
                Ok(ProductMarketStats {
                    product_id: spec.id.into(),
                    rfq_count: accumulator.rfq_count,
                    rfq_quantity: accumulator.rfq_quantity,
                    order_count: accumulator.order_count,
                    order_quantity: accumulator.order_quantity,
                    minimum_winning_unit_price: accumulator.winning_min,
                    maximum_winning_unit_price: accumulator.winning_max,
                    mean_winning_unit_price: mean,
                })
            })
            .collect::<Result<Vec<_>, GameError>>()?;
        let components = COMPONENTS
            .iter()
            .map(|spec| {
                let accumulator = &self.component_market[spec.id];
                let price_mean = if accumulator.ordered_quantity == 0 {
                    None
                } else {
                    Some(
                        i64::try_from(crate::arithmetic::round_half_even(
                            u128::try_from(accumulator.ordered_price_sum)
                                .map_err(|_| ArithmeticError::Overflow)?,
                            u128::from(accumulator.ordered_quantity),
                        )?)
                        .map_err(|_| ArithmeticError::Overflow)?,
                    )
                };
                Ok(ComponentMarketStats {
                    component_id: spec.id.into(),
                    ordered_quantity: accumulator.ordered_quantity,
                    delivered_quantity: accumulator.delivered_quantity,
                    mean_ordered_unit_price: price_mean,
                })
            })
            .collect::<Result<Vec<_>, GameError>>()?;
        let supplier_lines = self
            .capacity_market
            .iter()
            .map(|((supplier_id, component_id), accumulator)| {
                let mean_capacity_nano = if accumulator.capacity_count == 0 {
                    None
                } else {
                    Some(
                        u64::try_from(crate::arithmetic::round_half_even(
                            u128::from(accumulator.capacity_sum),
                            u128::from(accumulator.capacity_count),
                        )?)
                        .map_err(|_| ArithmeticError::Overflow)?,
                    )
                };
                Ok(CapacityMarketStats {
                    supplier_id: supplier_id.clone(),
                    component_id: component_id.clone(),
                    mean_capacity_nano,
                })
            })
            .collect::<Result<Vec<_>, GameError>>()?;
        let report = MarketReport {
            start_day: self.market_start_day,
            end_day: self.day,
            products,
            components,
            supplier_lines,
        };
        self.emit_visible(
            "report-published",
            "customer-settlement",
            Value::Map(vec![("report".into(), market_report_value(&report))]),
            None,
            EventVisibility::Public,
        )?;
        self.current_market_report = Some(report);
        self.product_market
            .values_mut()
            .for_each(|value| *value = ProductAccumulator::default());
        self.component_market
            .values_mut()
            .for_each(|value| *value = ComponentAccumulator::default());
        self.capacity_market
            .values_mut()
            .for_each(|value| *value = CapacityAccumulator::default());
        self.market_start_day = self.day + 1;
        Ok(())
    }

    fn inventory_value_microcredits(&self, agent: usize) -> Result<i128, GameError> {
        let component_value = self.agents[agent]
            .components
            .iter()
            .map(|(id, quantity)| {
                i128::from(*quantity)
                    * i128::from(
                        COMPONENTS
                            .iter()
                            .find(|component| component.id == id)
                            .expect("component inventory uses the catalog")
                            .base_microcredits,
                    )
            })
            .sum::<i128>();
        let pc_value = self.agents[agent]
            .pcs
            .iter()
            .map(|(id, quantity)| {
                let product = PRODUCTS
                    .iter()
                    .find(|product| product.id == id)
                    .expect("PC inventory uses the catalog");
                i128::from(*quantity)
                    * product
                        .components
                        .iter()
                        .map(|component| {
                            i128::from(
                                COMPONENTS
                                    .iter()
                                    .find(|candidate| candidate.id == *component)
                                    .expect("BOM component uses the catalog")
                                    .base_microcredits,
                            )
                        })
                        .sum::<i128>()
            })
            .sum::<i128>();
        component_value
            .checked_add(pc_value)
            .ok_or_else(|| GameError::from(ArithmeticError::Overflow))
    }

    fn inventory_close(&mut self) -> Result<(), GameError> {
        self.phase_open("inventory-close")?;
        let historical_storage = self.plan.historical_factors().financial_carry;
        for agent in 0..6 {
            let pending_components = std::mem::take(&mut self.pending_components[agent]);
            self.pending_components[agent] = COMPONENTS
                .iter()
                .map(|component| (component.id.into(), 0))
                .collect();
            for (id, quantity) in pending_components {
                if quantity == 0 {
                    continue;
                }
                let before = self.agents[agent].components[&id];
                let after = before
                    .checked_add(quantity)
                    .ok_or(ArithmeticError::Overflow)?;
                self.agents[agent].components.insert(id.clone(), after);
                self.inventory_transition(
                    "inventory-close",
                    agent,
                    "pending-component",
                    &id,
                    quantity,
                    0,
                )?;
                self.inventory_transition(
                    "inventory-close",
                    agent,
                    "component",
                    &id,
                    before,
                    after,
                )?;
            }
            let pending_pcs = std::mem::take(&mut self.pending_pcs[agent]);
            self.pending_pcs[agent] = PRODUCTS
                .iter()
                .map(|product| (product.id.into(), 0))
                .collect();
            for (id, quantity) in pending_pcs {
                if quantity == 0 {
                    continue;
                }
                let before = self.agents[agent].pcs[&id];
                let after = before
                    .checked_add(quantity)
                    .ok_or(ArithmeticError::Overflow)?;
                self.agents[agent].pcs.insert(id.clone(), after);
                self.inventory_transition(
                    "inventory-close",
                    agent,
                    "pending-pc",
                    &id,
                    quantity,
                    0,
                )?;
                self.inventory_transition("inventory-close", agent, "pc", &id, before, after)?;
            }
            if historical_storage {
                continue;
            }
            let inventory_value = self.inventory_value_microcredits(agent)?;
            let charge = storage_charge(
                u128::try_from(inventory_value).map_err(|_| ArithmeticError::Overflow)?,
                self.plan.storage_rate_ppb,
            )?;
            let cause_id = format!("storage-assessment/{}/{}", self.day, self.agents[agent].id);
            if charge > 0 {
                self.economic_event_with_id(
                    "storage-assessment",
                    "inventory-close",
                    &cause_id,
                    Some(&agent_state_id(&self.agents[agent].id)),
                    Value::Map(vec![
                        (
                            "agent_id".into(),
                            Value::Text(self.agents[agent].id.clone()),
                        ),
                        (
                            "inventory_value".into(),
                            Value::I64(
                                i64::try_from(inventory_value)
                                    .map_err(|_| ArithmeticError::Overflow)?,
                            ),
                        ),
                        (
                            "annual_rate_ppb".into(),
                            Value::U64(u64::from(self.plan.storage_rate_ppb)),
                        ),
                        ("amount".into(), Value::I64(charge)),
                    ]),
                    EventVisibility::Private(self.agents[agent].id.clone()),
                )?;
            }
            self.transfer(
                "inventory-close",
                agent,
                AgentTransfer::Debit("storage-market"),
                charge,
                "storage-charge",
                &cause_id,
            )?;
        }
        self.phase_close("inventory-close")
    }

    fn observation_phase(&mut self) -> Result<(), GameError> {
        self.phase_open("observation")?;
        let pre_observation = self.state_commitment("inventory-close");
        let daily_prices: Vec<_> = PRODUCTS
            .iter()
            .map(|product| {
                let (minimum, maximum) = self.daily_prices[product.id];
                DailyPriceRange {
                    product_id: product.id.into(),
                    minimum,
                    maximum,
                }
            })
            .collect();
        self.emit_visible(
            "report-published",
            "observation",
            Value::Map(vec![(
                "report".into(),
                daily_price_report_value(self.day, &daily_prices),
            )]),
            None,
            EventVisibility::Public,
        )?;
        self.observations.clear();
        for agent in 0..6 {
            let id_value = Value::Array(vec![
                Value::U64(u64::from(self.day)),
                Value::Text(self.agents[agent].id.clone()),
                Value::Bytes(pre_observation.to_vec()),
            ]);
            let id = format!("observation/{}", hex(&sha256(&id_value.encode())));
            self.agents[agent].last_observation_id = Some(id.clone());
            let supplier_orders = self
                .supplier_lines
                .iter()
                .flat_map(|line| line.orders.iter())
                .filter(|order| order.agent_id == self.agents[agent].id)
                .cloned()
                .collect();
            let supplier_reputations = self
                .supplier_lines
                .iter()
                .enumerate()
                .map(|(line_index, line)| {
                    let reputation = &line.reputations[&self.agents[agent].id];
                    SupplierReputation {
                        id: reputation_id(
                            &line.supplier_id,
                            &line.component_id,
                            &self.agents[agent].id,
                        ),
                        supplier_id: line.supplier_id.clone(),
                        component_id: line.component_id.clone(),
                        agent_id: self.agents[agent].id.clone(),
                        offered: reputation.offered,
                        purchased: reputation.purchased,
                        reputation_ppm: self.reputation_ppm(line_index, &self.agents[agent].id),
                    }
                })
                .collect();
            let ledger_since_previous = self
                .ledger_transactions
                .iter()
                .filter(|transaction| {
                    (transaction.day == self.day
                        || (transaction.day + 1 == self.day
                            && transaction.phase == "action-commit"))
                        && (transaction.source_account
                            == format!("agent-cash/{}", self.agents[agent].id)
                            || transaction.destination_account
                                == format!("agent-cash/{}", self.agents[agent].id))
                })
                .cloned()
                .collect();
            self.observations.push(Observation {
                agent_id: self.agents[agent].id.clone(),
                day: self.day,
                id,
                state_commitment: pre_observation,
                event_cursor: self.event_count,
                cash: self.agents[agent].cash,
                ledger_since_previous,
                component_inventory: self.agents[agent].components.clone(),
                pc_inventory: self.agents[agent].pcs.clone(),
                pending_components: self.pending_components[agent].clone(),
                pending_pcs: self.pending_pcs[agent].clone(),
                production_results: self.agents[agent].production_results.clone(),
                delivery_results: self.agents[agent].delivery_results.clone(),
                supplier_rfqs: self.agents[agent].supplier_rfqs.clone(),
                customer_rfqs: self
                    .customer_rfqs
                    .iter()
                    .filter(|rfq| rfq.id.starts_with(&format!("customer-rfq/{}/", self.day)))
                    .cloned()
                    .collect(),
                new_customer_orders: self.new_customer_orders[agent].clone(),
                customer_orders: self.agents[agent].customer_orders.clone(),
                supplier_offers: self
                    .supplier_lines
                    .iter()
                    .flat_map(|line| line.offers.iter())
                    .filter(|offer| {
                        offer.agent_id == self.agents[agent].id && offer.valid_day == self.day
                    })
                    .cloned()
                    .collect(),
                supplier_orders,
                supplier_deliveries: self.supplier_deliveries[agent].clone(),
                supplier_reputations,
                customer_settlements: self.customer_settlements[agent].clone(),
                previous_action: self.previous_action_for(&self.agents[agent].id),
                daily_prices: daily_prices.clone(),
                default_history: self.agents[agent].defaults.clone(),
                rejections: self.rejections[agent].clone(),
                market_report: self.current_market_report.clone(),
            });
        }
        self.emit_operator_summary("observation")?;
        let terminal_orders = self
            .agents
            .iter()
            .flat_map(|agent| {
                agent
                    .customer_orders
                    .iter()
                    .filter(|order| {
                        matches!(
                            order.status,
                            CustomerOrderStatus::Delivered | CustomerOrderStatus::Cancelled
                        )
                    })
                    .map(|order| {
                        (
                            agent.id.clone(),
                            order.id.clone(),
                            customer_order_value(order, self.day),
                        )
                    })
            })
            .collect::<Vec<_>>();
        for (agent_id, order_id, value) in terminal_orders {
            self.entity_event(
                "observation",
                "customer-order",
                &order_id,
                "removed",
                value,
                EventVisibility::Private(agent_id),
            )?;
        }
        for agent in &mut self.agents {
            agent.customer_orders.retain(|order| {
                !matches!(
                    order.status,
                    CustomerOrderStatus::Delivered | CustomerOrderStatus::Cancelled
                )
            });
        }
        let completed_schedule_results = self
            .agents
            .iter()
            .flat_map(|agent| {
                agent
                    .delivery_results
                    .iter()
                    .chain(agent.production_results.iter())
                    .cloned()
            })
            .collect::<Vec<_>>();
        for result in completed_schedule_results {
            self.entity_event(
                "observation",
                "schedule-result",
                &result.id,
                "removed",
                schedule_result_value(&result),
                EventVisibility::Private(result.agent_id.clone()),
            )?;
        }
        for agent in &mut self.agents {
            agent.delivery_results.clear();
            agent.production_results.clear();
        }
        let completed_deliveries = self
            .supplier_deliveries
            .iter()
            .enumerate()
            .flat_map(|(agent, deliveries)| {
                deliveries
                    .iter()
                    .cloned()
                    .map(move |delivery| (agent, delivery))
            })
            .collect::<Vec<_>>();
        for (agent, delivery) in completed_deliveries {
            self.entity_event(
                "observation",
                "supplier-delivery",
                &delivery.id,
                "removed",
                supplier_delivery_value(&delivery),
                EventVisibility::Private(self.agents[agent].id.clone()),
            )?;
        }
        let completed_settlements = self
            .customer_settlements
            .iter()
            .enumerate()
            .flat_map(|(agent, settlements)| {
                settlements
                    .iter()
                    .cloned()
                    .map(move |settlement| (agent, settlement))
            })
            .collect::<Vec<_>>();
        for (agent, settlement) in completed_settlements {
            self.entity_event(
                "observation",
                "customer-settlement",
                &settlement.id,
                "removed",
                customer_settlement_value(&settlement),
                EventVisibility::Private(self.agents[agent].id.clone()),
            )?;
        }
        if self.plan.rules().retire_delivered_supplier_orders {
            self.retire_delivered_supplier_orders()?;
        }
        self.supplier_deliveries.iter_mut().for_each(Vec::clear);
        self.customer_settlements.iter_mut().for_each(Vec::clear);
        self.new_customer_orders.iter_mut().for_each(Vec::clear);
        self.rejections.iter_mut().for_each(Vec::clear);
        self.current_market_report = None;
        self.daily_prices
            .values_mut()
            .for_each(|range| *range = (None, None));
        self.phase_close("observation")
    }

    /// Draft 3: a completely delivered supplier order has been carried by
    /// this observation together with its delivery, so it leaves
    /// authoritative state now, in supplier-line order and then line order.
    fn retire_delivered_supplier_orders(&mut self) -> Result<(), GameError> {
        let retired = self
            .supplier_lines
            .iter()
            .flat_map(|line| {
                line.orders
                    .iter()
                    .filter(|order| order.delivered == order.quantity)
                    .map(|order| {
                        (
                            order.clone(),
                            supplier_order_value(order, &line.supplier_id),
                        )
                    })
            })
            .collect::<Vec<_>>();
        for (order, value) in retired {
            let agent = self.agent_index(&order.agent_id)?;
            self.retired_supplier_units[agent] = self.retired_supplier_units[agent]
                .checked_add(u64::from(order.delivered))
                .ok_or(ArithmeticError::Overflow)?;
            self.entity_event(
                "observation",
                "supplier-order",
                &order.id,
                "removed",
                value,
                EventVisibility::Private(order.agent_id.clone()),
            )?;
        }
        for line in &mut self.supplier_lines {
            line.orders.retain(|order| order.delivered < order.quantity);
        }
        Ok(())
    }

    fn validate_inputs(
        &self,
        inputs: Vec<ActionInput>,
    ) -> Result<Vec<(GameActionSet, &'static str)>, GameError> {
        if inputs.len() != 6 {
            return Err(GameError::WrongActionCount(inputs.len()));
        }
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        for input in inputs {
            let (action, cause) = match input {
                ActionInput::Accepted(action) => (action, "agent-submission"),
                ActionInput::Default { agent_id, cause } => {
                    let observation = self
                        .observations
                        .iter()
                        .find(|obs| obs.agent_id == agent_id)
                        .ok_or_else(|| GameError::InvalidAction {
                            agent: agent_id.clone(),
                            reason: "unknown-agent",
                        })?;
                    (
                        GameActionSet::empty(&agent_id, &observation.id, self.day, 0),
                        cause.text(),
                    )
                }
            };
            if !seen.insert(action.agent_id.clone()) {
                return Err(GameError::InvalidAction {
                    agent: action.agent_id,
                    reason: "duplicate-agent",
                });
            }
            let observation = self
                .observations
                .iter()
                .find(|obs| obs.agent_id == action.agent_id)
                .ok_or_else(|| GameError::InvalidAction {
                    agent: action.agent_id.clone(),
                    reason: "unknown-agent",
                })?;
            if action.day != self.day
                || action.observation_id != observation.id
                || (cause == "agent-submission" && action.revision == 0)
            {
                return Err(GameError::InvalidAction {
                    agent: action.agent_id,
                    reason: "stale-day-observation-or-revision",
                });
            }
            let mut ids = BTreeSet::new();
            for id in action
                .customer_offers
                .iter()
                .map(|x| &x.command_id)
                .chain(action.supplier_rfqs.iter().map(|x| &x.command_id))
                .chain(action.supplier_orders.iter().map(|x| &x.command_id))
                .chain(action.production_schedule.iter().map(|x| &x.line_id))
                .chain(action.delivery_schedule.iter().map(|x| &x.line_id))
            {
                if !valid_id(id) || !ids.insert(id) {
                    return Err(GameError::InvalidAction {
                        agent: action.agent_id,
                        reason: "invalid-or-duplicate-id",
                    });
                }
            }
            if action.production_schedule.len() > 256 || action.delivery_schedule.len() > 4096 {
                return Err(GameError::InvalidAction {
                    agent: action.agent_id,
                    reason: "cardinality",
                });
            }
            result.push((action, cause));
        }
        result.sort_by(|a, b| a.0.agent_id.cmp(&b.0.agent_id));
        Ok(result)
    }

    fn action_commit(
        &mut self,
        actions: Vec<(GameActionSet, &'static str)>,
    ) -> Result<(), GameError> {
        self.phase_open("action-commit")?;
        // Supplier orders precede expiry and the new sealed RFQ batch.
        let mut supplier_commands = Vec::new();
        for (action, cause) in &actions {
            let agent = self.agent_index(&action.agent_id)?;
            if *cause != "agent-submission" {
                self.agents[agent].defaults.push((
                    self.day,
                    match *cause {
                        "timeout" => DefaultCause::Timeout,
                        "process-exit" => DefaultCause::ProcessExit,
                        "resource-limit" => DefaultCause::ResourceLimit,
                        "transport-error" => DefaultCause::TransportError,
                        _ => DefaultCause::Missing,
                    },
                ));
            }
            for command in &action.supplier_orders {
                if let Some((line, offer)) = self.supplier_offer_location(agent, &command.offer_id)
                {
                    supplier_commands.push((line, agent, offer.group_id.clone(), command.clone()));
                } else {
                    self.reject_command(
                        agent,
                        &command.command_id,
                        "expired",
                        "supplier_orders.offer_id",
                        "offer is unknown, expired, or belongs to another agent",
                    )?;
                }
            }
        }
        let mut group_counts = BTreeMap::new();
        for (_, agent, group, _) in &supplier_commands {
            *group_counts.entry((*agent, group.clone())).or_insert(0_u32) += 1;
        }
        if !self.plan.historical_factors().ordering_fairness {
            supplier_commands.sort_by(|left, right| {
                let left_line = &self.supplier_lines[left.0];
                let right_line = &self.supplier_lines[right.0];
                (
                    &left_line.supplier_id,
                    &left_line.component_id,
                    &self.agents[left.1].id,
                    &left.3.offer_id,
                )
                    .cmp(&(
                        &right_line.supplier_id,
                        &right_line.component_id,
                        &self.agents[right.1].id,
                        &right.3.offer_id,
                    ))
            });
        }
        let mut accepted_by_line: BTreeMap<usize, Vec<(usize, SupplierOrderCommand)>> =
            BTreeMap::new();
        for (line, agent, group, command) in supplier_commands {
            if group_counts[&(agent, group)] > 1 {
                self.reject_command(
                    agent,
                    &command.command_id,
                    "cardinality",
                    "supplier_orders",
                    "at most one offer in an alternative group may be ordered",
                )?;
            } else {
                accepted_by_line
                    .entry(line)
                    .or_default()
                    .push((agent, command));
            }
        }
        for (line, commands) in accepted_by_line {
            let context = format!(
                "{}/{}",
                self.supplier_lines[line].supplier_id, self.supplier_lines[line].component_id
            );
            let permutation = self.uniform_permutation(
                "action-commit",
                "supplier-order-acceptance",
                &context,
                commands
                    .iter()
                    .map(|(_, command)| command.offer_id.clone())
                    .collect(),
            )?;
            let mut by_offer = commands
                .into_iter()
                .map(|(agent, command)| (command.offer_id.clone(), (agent, command)))
                .collect::<BTreeMap<_, _>>();
            for offer_id in permutation {
                let (agent, command) = by_offer
                    .remove(&offer_id)
                    .expect("permuted supplier offer is accepted on this line");
                self.accept_supplier_order(agent, &command)?;
            }
        }
        self.expire_supplier_offers()?;
        for (action, _) in &actions {
            let agent = self.agent_index(&action.agent_id)?;
            let mut customer_offer_counts = BTreeMap::new();
            for offer in &action.customer_offers {
                *customer_offer_counts
                    .entry(offer.rfq_id.as_str())
                    .or_insert(0_u32) += 1;
            }
            for offer in &action.customer_offers {
                let matching = self.customer_rfqs.iter().find(|rfq| rfq.id == offer.rfq_id);
                if customer_offer_counts[offer.rfq_id.as_str()] > 1 {
                    self.reject_command(
                        agent,
                        &offer.command_id,
                        "cardinality",
                        "customer_offers",
                        "at most one customer offer per RFQ is permitted",
                    )?;
                } else if self.day >= 219 {
                    self.reject_command(
                        agent,
                        &offer.command_id,
                        "later-effect",
                        "customer_offers",
                        "day-219 customer offers cannot take effect",
                    )?;
                } else if matching.is_none() {
                    self.reject_command(
                        agent,
                        &offer.command_id,
                        "unknown-reference",
                        "customer_offers.rfq_id",
                        "customer RFQ is not visible and active",
                    )?;
                } else if offer.unit_price < 0
                    || matching.is_some_and(|rfq| offer.unit_price > rfq.reserve)
                {
                    self.reject_command(
                        agent,
                        &offer.command_id,
                        "out-of-range",
                        "customer_offers.unit_price",
                        "unit price must be non-negative and at most reserve",
                    )?;
                } else {
                    let award_day = self.day + 1;
                    let bid_id = customer_bid_id(award_day, &action.agent_id, offer);
                    self.entity_event(
                        "action-commit",
                        "customer-bid",
                        &bid_id,
                        "created",
                        customer_bid_value(award_day, &action.agent_id, offer),
                        EventVisibility::Private(action.agent_id.clone()),
                    )?;
                    self.customer_bids
                        .push((award_day, action.agent_id.clone(), offer.clone()));
                }
            }
            self.agents[agent].production_schedule.clear();
            for line in &action.production_schedule {
                let (accepted, code, detail) = if self.day >= 219 {
                    (
                        false,
                        "later-effect",
                        "day-219 production cannot enter inventory",
                    )
                } else if line.quantity == 0 {
                    (
                        false,
                        "out-of-range",
                        "production quantity must be positive",
                    )
                } else if !PRODUCTS.iter().any(|product| product.id == line.product_id) {
                    (false, "unknown-reference", "product does not exist")
                } else {
                    (true, "", "")
                };
                if accepted {
                    self.agents[agent].production_schedule.push(line.clone());
                } else {
                    self.reject_command(agent, &line.line_id, code, "production_schedule", detail)?;
                }
            }
            self.agents[agent].delivery_schedule.clear();
            let one_line_per_order = self.plan.rules().one_delivery_line_per_order;
            let mut scheduled_orders = BTreeSet::new();
            for line in &action.delivery_schedule {
                let ordered_quantity = self.agents[agent]
                    .customer_orders
                    .iter()
                    .find(|order| {
                        order.id == line.customer_order_id
                            && order.status == CustomerOrderStatus::Open
                    })
                    .map(|order| order.quantity);
                let (accepted, code, detail) = if self.day >= 219 {
                    (false, "later-effect", "day-219 delivery cannot execute")
                } else if line.quantity == 0 {
                    (false, "out-of-range", "delivery quantity must be positive")
                } else if ordered_quantity.is_none() {
                    (false, "unknown-reference", "customer order does not exist")
                } else if ordered_quantity != Some(line.quantity) {
                    (
                        false,
                        "term-mismatch",
                        "delivery must use the exact full quantity",
                    )
                } else if one_line_per_order
                    && !scheduled_orders.insert(line.customer_order_id.as_str())
                {
                    (
                        false,
                        "cardinality",
                        "at most one delivery line per customer order is permitted",
                    )
                } else {
                    (true, "", "")
                };
                if accepted {
                    self.agents[agent].delivery_schedule.push(line.clone());
                } else {
                    self.reject_command(agent, &line.line_id, code, "delivery_schedule", detail)?;
                }
            }

            let over_limit = self.supplier_rfq_limit_violations(action);
            for rfq in &action.supplier_rfqs {
                let (code, detail) = match self.supplier_rfq_rejection(rfq) {
                    Some((code, detail)) => (Some(code), detail),
                    None if over_limit.contains(&rfq.command_id) => (
                        Some("cardinality"),
                        "at most five positive RFQs and one probe per supplier component and day",
                    ),
                    None => (None, ""),
                };
                if let Some(code) = code {
                    self.reject_command(agent, &rfq.command_id, code, "supplier_rfqs", detail)?;
                }
            }
        }
        self.clear_supplier_rfqs(&actions)?;
        for (action, cause) in actions {
            let digest = sha256(&action.value().encode());
            let committed_state = Value::Map(vec![
                ("day".into(), Value::U64(u64::from(self.day))),
                ("agent_id".into(), Value::Text(action.agent_id.clone())),
                ("action_digest".into(), Value::Bytes(digest.to_vec())),
                ("cause".into(), Value::Text(cause.into())),
            ]);
            self.committed_actions.push(committed_state.clone());
            self.transcript.push(Value::Map(vec![
                ("state".into(), committed_state),
                ("action".into(), action.value()),
            ]));
            self.emit(
                "action-committed",
                "action-commit",
                Value::Map(vec![
                    ("agent_id".into(), Value::Text(action.agent_id)),
                    (
                        "disposition".into(),
                        Value::Map(vec![
                            ("day".into(), Value::U64(u64::from(self.day))),
                            ("action_digest".into(), Value::Bytes(digest.to_vec())),
                            ("cause".into(), Value::Text(cause.into())),
                            ("revision".into(), Value::U64(u64::from(action.revision))),
                        ]),
                    ),
                ]),
                None,
            )?;
        }
        self.phase_close("action-commit")
    }

    /// The profile-independent rejection of one supplier RFQ, if any.
    fn supplier_rfq_rejection(
        &self,
        rfq: &SupplierRfqCommand,
    ) -> Option<(&'static str, &'static str)> {
        let mapped = COMPONENTS.iter().any(|component| {
            component.id == rfq.component_id
                && component.suppliers.contains(&rfq.supplier_id.as_str())
        });
        if self.day >= 219 {
            Some(("later-effect", "day-219 supplier RFQ cannot take effect"))
        } else if !mapped {
            Some((
                "unknown-reference",
                "supplier does not supply this component",
            ))
        } else if rfq.quantity > 100_000 || rfq.due_day < self.day + 2 || rfq.due_day > 219 {
            Some((
                "out-of-range",
                "quantity or due day is outside supplier limits",
            ))
        } else if rfq.max_unit_price.is_some_and(|price| price < 0) {
            Some(("out-of-range", "maximum unit price must be non-negative"))
        } else {
            None
        }
    }

    /// Draft 3: command ids of the agent's otherwise valid supplier RFQs in a
    /// supplier-component class that exceeds the daily limit of five positive
    /// requests or one zero-quantity probe. Every RFQ of an exceeded class is
    /// rejected, so command order never selects which requests survive.
    fn supplier_rfq_limit_violations(&self, action: &GameActionSet) -> BTreeSet<String> {
        if !self.plan.rules().supplier_rfq_pair_limit {
            return BTreeSet::new();
        }
        let mut classes: BTreeMap<(&str, &str, bool), Vec<&str>> = BTreeMap::new();
        for rfq in &action.supplier_rfqs {
            if self.supplier_rfq_rejection(rfq).is_none() {
                classes
                    .entry((&rfq.supplier_id, &rfq.component_id, rfq.quantity == 0))
                    .or_default()
                    .push(&rfq.command_id);
            }
        }
        classes
            .into_iter()
            .filter(|((_, _, probe), ids)| ids.len() > if *probe { 1 } else { 5 })
            .flat_map(|(_, ids)| ids.into_iter().map(str::to_owned))
            .collect()
    }

    fn accept_supplier_order(
        &mut self,
        agent: usize,
        command: &SupplierOrderCommand,
    ) -> Result<(), GameError> {
        let mut found = None;
        for (line_index, line) in self.supplier_lines.iter().enumerate() {
            if let Some(offer_index) = line.offers.iter().position(|offer| {
                offer.id == command.offer_id
                    && offer.agent_id == self.agents[agent].id
                    && offer.valid_day == self.day
                    && !matches!(
                        offer.kind,
                        SupplierOfferKind::None | SupplierOfferKind::Probe
                    )
            }) {
                found = Some((line_index, offer_index));
                break;
            }
        }
        let Some((line_index, offer_index)) = found else {
            return Ok(());
        };
        let offer = self.supplier_lines[line_index].offers[offer_index].clone();
        if self.supplier_lines[line_index]
            .orders
            .iter()
            .any(|order| order.offer_id == offer.id)
        {
            return Ok(());
        }
        let total = i128::from(offer.quantity) * i128::from(offer.unit_price);
        let down = i64::try_from(mul_rate(
            u128::try_from(total).map_err(|_| ArithmeticError::Overflow)?,
            100_000_000,
        )?)
        .map_err(|_| ArithmeticError::Overflow)?;
        let ordinal = self.supplier_lines[line_index].next_ordinal;
        self.supplier_lines[line_index].next_ordinal += 1;
        let id = format!(
            "supplier-order/{}",
            hex(&sha256(
                &Value::Array(vec![
                    Value::Text(self.agents[agent].id.clone()),
                    Value::Text(offer.id.clone())
                ])
                .encode()
            ))
        );
        let order = SupplierOrder {
            id,
            offer_id: offer.id,
            agent_id: self.agents[agent].id.clone(),
            supplier_id: offer.supplier_id,
            component_id: offer.component_id,
            quantity: offer.quantity,
            due_day: offer.due_day,
            unit_price: offer.unit_price,
            down_payment: down,
            delivered: 0,
            ordinal,
        };
        self.supplier_lines[line_index].orders.push(order.clone());
        self.entity_event(
            "action-commit",
            "supplier-order",
            &order.id,
            "created",
            supplier_order_value(&order, &self.supplier_lines[line_index].supplier_id),
            EventVisibility::Private(self.agents[agent].id.clone()),
        )?;
        self.transfer(
            "action-commit",
            agent,
            AgentTransfer::Debit("supplier-market"),
            down,
            "supplier-down-payment",
            &order.id,
        )?;
        let market = self
            .component_market
            .get_mut(&self.supplier_lines[line_index].component_id)
            .unwrap();
        market.ordered_quantity += u64::from(offer.quantity);
        market.ordered_price_sum += i128::from(offer.quantity) * i128::from(offer.unit_price);
        Ok(())
    }

    fn supplier_offer_location(
        &self,
        agent: usize,
        offer_id: &str,
    ) -> Option<(usize, SupplierOffer)> {
        self.supplier_lines
            .iter()
            .enumerate()
            .find_map(|(line_index, line)| {
                line.offers
                    .iter()
                    .find(|offer| {
                        offer.id == offer_id
                            && offer.agent_id == self.agents[agent].id
                            && offer.valid_day == self.day
                            && !matches!(
                                offer.kind,
                                SupplierOfferKind::None | SupplierOfferKind::Probe
                            )
                    })
                    .cloned()
                    .map(|offer| (line_index, offer))
            })
    }

    fn expire_supplier_offers(&mut self) -> Result<(), GameError> {
        let mut changes = Vec::new();
        let mut removed_offers = Vec::new();
        for line in &mut self.supplier_lines {
            let mut groups: BTreeMap<(String, String), Vec<SupplierOffer>> = BTreeMap::new();
            for offer in line
                .offers
                .iter()
                .filter(|offer| offer.valid_day == self.day)
            {
                groups
                    .entry((offer.agent_id.clone(), offer.group_id.clone()))
                    .or_default()
                    .push(offer.clone());
            }
            for ((agent_id, _), offers) in groups {
                let purchased_offer = offers.iter().find_map(|offer| {
                    line.orders
                        .iter()
                        .find(|order| order.offer_id == offer.id)
                        .map(|order| (offer, order.quantity))
                });
                let purchased = purchased_offer.map_or(0, |(_, quantity)| quantity);
                let mut exposure = offers.iter().map(|offer| offer.exposure).max().unwrap_or(0);
                if let Some((offer, quantity)) = purchased_offer
                    && offer.kind == SupplierOfferKind::Complete
                {
                    exposure = exposure.max(quantity);
                }
                let reputation = line.reputations.get_mut(&agent_id).unwrap();
                reputation.offered += u64::from(exposure);
                reputation.purchased += u64::from(purchased);
                changes.push((
                    line.supplier_id.clone(),
                    line.component_id.clone(),
                    agent_id,
                    reputation.offered,
                    reputation.purchased,
                ));
            }
            removed_offers.extend(
                line.offers
                    .iter()
                    .filter(|offer| offer.valid_day <= self.day)
                    .cloned(),
            );
            line.offers.retain(|offer| offer.valid_day > self.day);
        }
        for offer in removed_offers {
            self.entity_event(
                "action-commit",
                "supplier-offer",
                &offer.id,
                "removed",
                supplier_offer_value(&offer),
                EventVisibility::Private(offer.agent_id.clone()),
            )?;
        }
        let live_rfq_ids = self
            .supplier_lines
            .iter()
            .flat_map(|line| line.offers.iter().map(|offer| offer.rfq_id.clone()))
            .collect::<BTreeSet<_>>();
        let removed_rfqs = self
            .agents
            .iter()
            .flat_map(|agent| {
                agent
                    .supplier_rfqs
                    .iter()
                    .filter(|rfq| !live_rfq_ids.contains(&rfq.id))
                    .cloned()
            })
            .collect::<Vec<_>>();
        for rfq in removed_rfqs {
            self.entity_event(
                "action-commit",
                "supplier-rfq",
                &rfq.id,
                "removed",
                supplier_rfq_value(&rfq),
                EventVisibility::Private(rfq.agent_id.clone()),
            )?;
        }
        for agent in &mut self.agents {
            agent
                .supplier_rfqs
                .retain(|rfq| live_rfq_ids.contains(&rfq.id));
        }
        for (supplier_id, component_id, agent_id, offered, purchased) in changes {
            let line = self
                .supplier_lines
                .iter()
                .position(|line| {
                    line.supplier_id == supplier_id && line.component_id == component_id
                })
                .unwrap();
            let value = SupplierReputation {
                id: reputation_id(&supplier_id, &component_id, &agent_id),
                supplier_id: supplier_id.clone(),
                component_id: component_id.clone(),
                agent_id: agent_id.clone(),
                offered,
                purchased,
                reputation_ppm: self.reputation_ppm(line, &agent_id),
            };
            self.entity_event(
                "action-commit",
                "supplier-reputation",
                &value.id,
                "updated",
                supplier_reputation_value(&value),
                EventVisibility::Private(agent_id),
            )?;
        }
        Ok(())
    }

    fn clear_supplier_rfqs(
        &mut self,
        actions: &[(GameActionSet, &'static str)],
    ) -> Result<(), GameError> {
        if self.day >= 219 {
            return Ok(());
        }
        let over_limit = actions
            .iter()
            .flat_map(|(action, _)| {
                self.supplier_rfq_limit_violations(action)
                    .into_iter()
                    .map(|command_id| (action.agent_id.clone(), command_id))
            })
            .collect::<BTreeSet<_>>();
        for line_index in 0..self.supplier_lines.len() {
            let supplier = self.supplier_lines[line_index].supplier_id.clone();
            let component = self.supplier_lines[line_index].component_id.clone();
            let base = COMPONENTS
                .iter()
                .find(|x| x.id == component)
                .unwrap()
                .base_microcredits;
            let mut requests = Vec::new();
            for (action, _) in actions {
                for command in action.supplier_rfqs.iter().filter(|rfq| {
                    rfq.supplier_id == supplier
                        && rfq.component_id == component
                        && rfq.due_day >= self.day + 2
                        && rfq.due_day <= 219
                        && rfq.quantity <= 100_000
                        && !over_limit.contains(&(action.agent_id.clone(), rfq.command_id.clone()))
                }) {
                    requests.push((
                        action.agent_id.clone(),
                        command.clone(),
                        supplier_rfq_id(&action.agent_id, &command.supplier_id, &command.rfq_id),
                    ));
                }
            }
            if !self.plan.historical_factors().ordering_fairness {
                requests.sort_by(|a, b| {
                    (a.1.due_day, &a.0, &a.1.rfq_id).cmp(&(b.1.due_day, &b.0, &b.1.rfq_id))
                });
            }
            if requests.is_empty() {
                continue;
            }
            let mut supply = [0_u64; DAYS as usize];
            let mut committed = [0_u64; DAYS as usize];
            let mut expected = self.supplier_lines[line_index].actual_capacity;
            let mut cumulative_supply = u64::from(self.supplier_lines[line_index].inventory);
            for h in 1..DAYS - self.day {
                expected = u32::try_from(crate::arithmetic::round_half_even(
                    u128::from(99 * expected + 550),
                    100,
                )?)
                .unwrap();
                if h >= 2 {
                    let willing = if h <= 20 {
                        expected
                    } else {
                        u32::try_from(u128::from(expected) * u128::from(220 - h) / 200).unwrap()
                    };
                    cumulative_supply += u64::from(willing);
                }
                supply[usize::try_from(self.day + h).unwrap()] = cumulative_supply;
            }
            for day in self.day + 2..DAYS {
                committed[usize::try_from(day).unwrap()] = self.supplier_lines[line_index]
                    .orders
                    .iter()
                    .filter(|order| order.delivered < order.quantity && order.due_day <= day)
                    .map(|order| u64::from(order.quantity - order.delivered))
                    .sum();
            }
            let budgets =
                std::array::from_fn(|index| supply[index].saturating_sub(committed[index]));
            let historical_supplier = self.plan.historical_factors().supplier_market;
            let batch_requested_quantity =
                requests.iter().try_fold(0_u64, |total, (_, rfq, _)| {
                    total
                        .checked_add(u64::from(rfq.quantity))
                        .ok_or(ArithmeticError::Overflow)
                })?;
            let probe_count = u64::try_from(
                requests
                    .iter()
                    .filter(|(_, rfq, _)| rfq.quantity == 0)
                    .count(),
            )
            .map_err(|_| ArithmeticError::Overflow)?;
            let actual_capacity = self.supplier_lines[line_index].actual_capacity;
            let mut request_prices = BTreeMap::new();
            for (agent, request, engine_rfq) in &requests {
                let day = usize::try_from(request.due_day).unwrap();
                let price = if historical_supplier {
                    historical_supplier_price(
                        base,
                        supply[day],
                        committed[day],
                        batch_requested_quantity,
                        probe_count,
                        actual_capacity,
                        self.reputation_ppm(line_index, agent),
                    )?
                } else {
                    supplier_posted_price(base, supply[day], committed[day])?
                };
                request_prices.insert(engine_rfq.clone(), price);
            }
            let eligible: Vec<_> = requests
                .iter()
                .filter(|(_, request, engine_rfq)| {
                    let posted = request_prices[engine_rfq];
                    request.quantity > 0
                        && request
                            .max_unit_price
                            .is_none_or(|maximum| maximum >= posted)
                })
                .map(|(agent, r, engine_rfq)| AllocationRequest {
                    id: engine_rfq.clone(),
                    quantity: r.quantity,
                    reputation_ppm: self.reputation_ppm(line_index, agent),
                    due_day: r.due_day,
                    symmetry_rank: 0,
                })
                .collect::<Vec<_>>();
            let rank_order = self.uniform_permutation(
                "action-commit",
                "supplier-rfq-allocation",
                &format!("{supplier}/{component}"),
                eligible.iter().map(|request| request.id.clone()).collect(),
            )?;
            let ranks = rank_order
                .iter()
                .enumerate()
                .map(|(rank, id)| (id.clone(), u64::try_from(rank).unwrap()))
                .collect::<BTreeMap<_, _>>();
            let eligible = eligible
                .into_iter()
                .map(|mut request| {
                    request.symmetry_rank = ranks[&request.id];
                    request
                })
                .collect::<Vec<_>>();
            let mut shadow = [0_u64; DAYS as usize];
            let allocations = if historical_supplier {
                let by_id = eligible
                    .iter()
                    .map(|request| (request.id.clone(), request))
                    .collect::<BTreeMap<_, _>>();
                let mut allocations = BTreeMap::new();
                for request_id in &rank_order {
                    let request = by_id[request_id];
                    let quantity = maximum_cumulative_quantity(
                        &shadow,
                        &budgets,
                        request.due_day,
                        u64::from(request.quantity),
                    );
                    let quantity = u32::try_from(quantity).unwrap();
                    shadow[usize::try_from(request.due_day).unwrap()] += u64::from(quantity);
                    allocations.insert(request.id.clone(), quantity);
                }
                allocations
            } else {
                let allocation_values = weighted_cumulative_allocation(&eligible, &budgets);
                let allocations = eligible
                    .iter()
                    .zip(allocation_values)
                    .map(|(request, quantity)| (request.id.clone(), quantity))
                    .collect::<BTreeMap<_, _>>();
                for request in &eligible {
                    shadow[usize::try_from(request.due_day).unwrap()] +=
                        u64::from(allocations[&request.id]);
                }
                allocations
            };
            let mut alternatives = BTreeMap::new();
            if historical_supplier {
                let empty_shadow = [0_u64; DAYS as usize];
                for (agent, request, engine_rfq) in &requests {
                    if request.quantity == 0
                        || allocations.get(engine_rfq).copied().unwrap_or(0) == request.quantity
                    {
                        continue;
                    }
                    for due_day in request.due_day + 1..DAYS {
                        let index = usize::try_from(due_day).unwrap();
                        let posted = historical_supplier_price(
                            base,
                            supply[index],
                            committed[index],
                            batch_requested_quantity,
                            probe_count,
                            actual_capacity,
                            self.reputation_ppm(line_index, agent),
                        )?;
                        if request
                            .max_unit_price
                            .is_some_and(|maximum| maximum < posted)
                        {
                            continue;
                        }
                        if maximum_cumulative_quantity(
                            &empty_shadow,
                            &budgets,
                            due_day,
                            u64::from(request.quantity),
                        ) == u64::from(request.quantity)
                        {
                            alternatives.insert(engine_rfq.clone(), (due_day, posted));
                            break;
                        }
                    }
                }
            } else {
                let mut limited: Vec<_> = eligible
                    .iter()
                    .filter(|request| allocations[&request.id] < request.quantity)
                    .collect();
                limited.sort_by(|left, right| {
                    allocation_priority(left, allocations[&left.id], right, allocations[&right.id])
                });
                for request in limited {
                    let remainder = request.quantity - allocations[&request.id];
                    let original = requests
                        .iter()
                        .find(|(_, _, engine_rfq)| *engine_rfq == request.id)
                        .unwrap();
                    for due_day in request.due_day + 1..DAYS {
                        let index = usize::try_from(due_day).unwrap();
                        let posted = supplier_posted_price(base, supply[index], committed[index])?;
                        if original
                            .1
                            .max_unit_price
                            .is_some_and(|maximum| maximum < posted)
                        {
                            continue;
                        }
                        if cumulative_feasible(&shadow, &budgets, due_day, u64::from(remainder)) {
                            shadow[index] += u64::from(remainder);
                            alternatives.insert(request.id.clone(), (due_day, posted));
                            break;
                        }
                    }
                }
            }
            for (agent, rfq, engine_rfq) in requests {
                let quantity = allocations.get(&engine_rfq).copied().unwrap_or(0);
                let posted = request_prices[&engine_rfq];
                let kind = if rfq.quantity == 0 {
                    SupplierOfferKind::Probe
                } else if quantity > 0 {
                    SupplierOfferKind::OnDate
                } else {
                    SupplierOfferKind::None
                };
                let reason = matches!(kind, SupplierOfferKind::None).then_some(
                    if rfq.max_unit_price.is_some_and(|max| max < posted) {
                        "term-mismatch"
                    } else {
                        "insufficient-capacity"
                    },
                );
                let accepted_rfq = SupplierRfq {
                    id: engine_rfq.clone(),
                    client_rfq_id: rfq.rfq_id.clone(),
                    agent_id: agent.clone(),
                    supplier_id: supplier.clone(),
                    component_id: component.clone(),
                    quantity: rfq.quantity,
                    due_day: rfq.due_day,
                    max_unit_price: rfq.max_unit_price,
                };
                let agent_index = self.agent_index(&agent)?;
                self.agents[agent_index]
                    .supplier_rfqs
                    .push(accepted_rfq.clone());
                self.entity_event(
                    "action-commit",
                    "supplier-rfq",
                    &engine_rfq,
                    "created",
                    supplier_rfq_value(&accepted_rfq),
                    EventVisibility::Private(agent.clone()),
                )?;
                self.economic_event(
                    "supplier-price",
                    "action-commit",
                    Some(&engine_rfq),
                    Value::Map(vec![("unit_price".into(), Value::I64(posted))]),
                    EventVisibility::Private(agent.clone()),
                )?;
                self.economic_event(
                    "supplier-allocation",
                    "action-commit",
                    Some(&engine_rfq),
                    Value::Map(vec![(
                        "on_date_quantity".into(),
                        Value::U64(u64::from(quantity)),
                    )]),
                    EventVisibility::Private(agent.clone()),
                )?;
                let group = format!(
                    "alternative-group/{}",
                    hex(&sha256(
                        &Value::Array(vec![Value::Text(engine_rfq.clone())]).encode()
                    ))
                );
                let exposure = if historical_supplier {
                    if rfq.quantity == 0 {
                        (actual_capacity / 20).max(1)
                    } else if quantity > 0 || alternatives.contains_key(&engine_rfq) {
                        quantity.max(rfq.quantity / 5)
                    } else {
                        0
                    }
                } else if rfq.quantity == 0 || reason.is_some() {
                    0
                } else {
                    quantity.max(rfq.quantity / 5)
                };
                let on_date = supplier_offer(
                    &engine_rfq,
                    &group,
                    &agent,
                    &supplier,
                    &component,
                    kind,
                    quantity,
                    rfq.due_day,
                    posted,
                    self.day + 1,
                    reason,
                    exposure,
                );
                self.entity_event(
                    "action-commit",
                    "supplier-offer",
                    &on_date.id,
                    "created",
                    supplier_offer_value(&on_date),
                    EventVisibility::Private(agent.clone()),
                )?;
                self.supplier_lines[line_index].offers.push(on_date);
                if let Some((alternative_day, alternative_price)) = alternatives.get(&engine_rfq) {
                    let complete = supplier_offer(
                        &engine_rfq,
                        &group,
                        &agent,
                        &supplier,
                        &component,
                        SupplierOfferKind::Complete,
                        rfq.quantity,
                        *alternative_day,
                        *alternative_price,
                        self.day + 1,
                        None,
                        exposure,
                    );
                    self.entity_event(
                        "action-commit",
                        "supplier-offer",
                        &complete.id,
                        "created",
                        supplier_offer_value(&complete),
                        EventVisibility::Private(agent.clone()),
                    )?;
                    self.supplier_lines[line_index].offers.push(complete);
                }
            }
        }
        Ok(())
    }

    fn reputation_ppm(&self, line: usize, agent: &str) -> u32 {
        let rep = &self.supplier_lines[line].reputations[agent];
        let apr = if COMPONENTS
            .iter()
            .find(|component| component.id == self.supplier_lines[line].component_id)
            .unwrap()
            .suppliers
            .len()
            == 1
        {
            750_000_000_u64
        } else {
            450_000_000
        };
        let numerator =
            1_000_000_u128 * u128::from(2_000 + rep.purchased + 100 * u64::from(self.day));
        let denominator = u128::from(
            2_000 + apr.saturating_mul(rep.offered) / 1_000_000_000 + 100 * u64::from(self.day),
        );
        u32::try_from((numerator / denominator).min(1_000_000)).unwrap()
    }

    fn final_settlement(&mut self) -> Result<(), GameError> {
        self.phase_open("final-settlement")?;
        for line_index in 0..self.supplier_lines.len() {
            let inventory = self.supplier_lines[line_index].inventory;
            let mut remaining = inventory;
            let mut allocations = Vec::new();
            if self.plan.historical_factors().terminal_settlement {
                let mut indices = self.supplier_lines[line_index]
                    .orders
                    .iter()
                    .enumerate()
                    .filter(|(_, order)| order.delivered < order.quantity)
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                indices.sort_by_key(|index| self.supplier_lines[line_index].orders[*index].ordinal);
                for index in indices {
                    let order = &self.supplier_lines[line_index].orders[index];
                    let delivered = remaining.min(order.quantity - order.delivered);
                    remaining -= delivered;
                    allocations.push((index, delivered));
                }
            } else {
                let mut groups: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
                for index in 0..self.supplier_lines[line_index].orders.len() {
                    let order = &self.supplier_lines[line_index].orders[index];
                    if order.delivered < order.quantity {
                        groups.entry(order.due_day).or_default().push(index);
                    }
                }
                for indices in groups.values() {
                    if remaining == 0 {
                        allocations.extend(indices.iter().map(|index| (*index, 0_u32)));
                        continue;
                    }
                    let wanted: u64 = indices
                        .iter()
                        .map(|index| {
                            let order = &self.supplier_lines[line_index].orders[*index];
                            u64::from(order.quantity - order.delivered)
                        })
                        .sum();
                    if u64::from(remaining) >= wanted {
                        for index in indices {
                            let order = &self.supplier_lines[line_index].orders[*index];
                            allocations.push((*index, order.quantity - order.delivered));
                        }
                        remaining -= u32::try_from(wanted).unwrap();
                    } else {
                        let mut group_allocations = Vec::new();
                        let mut floor_sum = 0_u32;
                        for index in indices {
                            let order = &self.supplier_lines[line_index].orders[*index];
                            let order_wanted = u64::from(order.quantity - order.delivered);
                            let numerator = u64::from(remaining) * order_wanted;
                            let floor = u32::try_from(numerator / wanted).unwrap();
                            floor_sum += floor;
                            group_allocations.push((*index, floor, numerator % wanted));
                        }
                        group_allocations.sort_by(|left, right| {
                            let left_order = &self.supplier_lines[line_index].orders[left.0];
                            let right_order = &self.supplier_lines[line_index].orders[right.0];
                            right
                                .2
                                .cmp(&left.2)
                                .then_with(|| left_order.ordinal.cmp(&right_order.ordinal))
                        });
                        let residual = remaining - floor_sum;
                        for (position, (index, floor, _)) in
                            group_allocations.into_iter().enumerate()
                        {
                            allocations.push((
                                index,
                                floor + u32::from(position < usize::try_from(residual).unwrap()),
                            ));
                        }
                        remaining = 0;
                    }
                }
            }
            for (index, delivered) in allocations {
                let order = self.supplier_lines[line_index].orders[index].clone();
                let wanted = order.quantity - order.delivered;
                let (retained, refund, debit) =
                    if self.plan.historical_factors().terminal_settlement {
                        historical_supplier_final_settlement(
                            delivered,
                            order.unit_price,
                            order.down_payment,
                        )?
                    } else {
                        supplier_final_settlement(
                            wanted,
                            delivered,
                            order.unit_price,
                            order.down_payment,
                        )?
                    };
                let agent = self.agent_index(&order.agent_id)?;
                if delivered > 0 {
                    let delivery_id = self.next_entity_id("supplier-delivery", "final-settlement");
                    let delivery = SupplierDelivery {
                        id: delivery_id.clone(),
                        order_id: order.id.clone(),
                        component_id: order.component_id.clone(),
                        quantity: delivered,
                        unit_price: order.unit_price,
                        invoice: debit,
                    };
                    self.supplier_deliveries[agent].push(delivery.clone());
                    self.entity_event(
                        "final-settlement",
                        "supplier-delivery",
                        &delivery_id,
                        "created",
                        supplier_delivery_value(&delivery),
                        EventVisibility::Private(order.agent_id.clone()),
                    )?;
                    self.transfer(
                        "final-settlement",
                        agent,
                        AgentTransfer::Debit("supplier-market"),
                        debit,
                        "supplier-delivery-balance",
                        &delivery_id,
                    )?;
                }
                if refund > 0 {
                    let refund_id = self.next_economic_id("supplier-refund", "final-settlement");
                    self.economic_event_with_id(
                        "supplier-refund",
                        "final-settlement",
                        &refund_id,
                        Some(&order.id),
                        Value::Map(vec![
                            ("order_id".into(), Value::Text(order.id.clone())),
                            (
                                "undelivered_quantity".into(),
                                Value::U64(u64::from(wanted - delivered)),
                            ),
                            ("amount".into(), Value::I64(refund)),
                        ]),
                        EventVisibility::Private(order.agent_id.clone()),
                    )?;
                    self.transfer(
                        "final-settlement",
                        agent,
                        AgentTransfer::Credit("supplier-market"),
                        refund,
                        "supplier-refund",
                        &refund_id,
                    )?;
                }
                if delivered > 0 {
                    let before = self.agents[agent].components[&order.component_id];
                    let after = before
                        .checked_add(delivered)
                        .ok_or(ArithmeticError::Overflow)?;
                    self.agents[agent]
                        .components
                        .insert(order.component_id.clone(), after);
                    self.inventory_transition(
                        "final-settlement",
                        agent,
                        "component",
                        &order.component_id,
                        before,
                        after,
                    )?;
                }
                self.supplier_lines[line_index].orders[index].delivered += delivered;
                self.supplier_lines[line_index].orders[index].down_payment = retained;
                let updated = self.supplier_lines[line_index].orders[index].clone();
                self.entity_event(
                    "final-settlement",
                    "supplier-order",
                    &updated.id,
                    "updated",
                    supplier_order_value(&updated, &self.supplier_lines[line_index].supplier_id),
                    EventVisibility::Private(updated.agent_id.clone()),
                )?;
            }
            if self.plan.historical_factors().terminal_settlement {
                remaining = 0;
            }
            self.supplier_lines[line_index].inventory = remaining;
            if inventory != remaining {
                let line_id = supplier_line_state_id(
                    &self.supplier_lines[line_index].supplier_id,
                    &self.supplier_lines[line_index].component_id,
                );
                self.economic_event(
                    "supplier-inventory",
                    "final-settlement",
                    Some(&line_id),
                    Value::Map(vec![
                        ("before".into(), Value::U64(u64::from(inventory))),
                        ("after".into(), Value::U64(u64::from(remaining))),
                    ]),
                    EventVisibility::Operator,
                )?;
            }
        }
        if self.plan.historical_factors().terminal_settlement {
            for agent in 0..self.agents.len() {
                for index in 0..self.agents[agent].customer_orders.len() {
                    if !matches!(
                        self.agents[agent].customer_orders[index].status,
                        CustomerOrderStatus::DeliveredPending { .. }
                    ) {
                        continue;
                    }
                    let amount = i64::try_from(
                        i128::from(self.agents[agent].customer_orders[index].quantity)
                            * i128::from(self.agents[agent].customer_orders[index].unit_price),
                    )
                    .map_err(|_| ArithmeticError::Overflow)?;
                    let order_id = self.agents[agent].customer_orders[index].id.clone();
                    self.agents[agent].customer_orders[index].status =
                        CustomerOrderStatus::Delivered;
                    let settlement = CustomerSettlement {
                        id: self.next_entity_id("customer-settlement", "final-settlement"),
                        order_id: order_id.clone(),
                        delivered: true,
                        payment: amount,
                        penalty: 0,
                        penalty_count: self.agents[agent].customer_orders[index].penalty_count,
                        cancelled: false,
                    };
                    self.customer_settlements[agent].push(settlement.clone());
                    self.entity_event(
                        "final-settlement",
                        "customer-order",
                        &order_id,
                        "updated",
                        customer_order_value(&self.agents[agent].customer_orders[index], self.day),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.entity_event(
                        "final-settlement",
                        "customer-settlement",
                        &settlement.id,
                        "created",
                        customer_settlement_value(&settlement),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    let payment_id = self.next_economic_id("customer-payment", "final-settlement");
                    self.economic_event_with_id(
                        "customer-payment",
                        "final-settlement",
                        &payment_id,
                        Some(&order_id),
                        Value::Map(vec![
                            ("settlement_id".into(), Value::Text(settlement.id.clone())),
                            ("amount".into(), Value::I64(amount)),
                        ]),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.transfer(
                        "final-settlement",
                        agent,
                        AgentTransfer::Credit("customer-market"),
                        amount,
                        "customer-payment",
                        &payment_id,
                    )?;
                }
            }
        }
        for agent in 0..6 {
            for index in 0..self.agents[agent].customer_orders.len() {
                while self.agents[agent].customer_orders[index].status == CustomerOrderStatus::Open
                    && self.agents[agent].customer_orders[index].penalty_count < 5
                {
                    let penalty = self.agents[agent].customer_orders[index].penalty;
                    let order_id = self.agents[agent].customer_orders[index].id.clone();
                    self.agents[agent].customer_orders[index].penalty_count += 1;
                    let penalty_count = self.agents[agent].customer_orders[index].penalty_count;
                    let cancelled = penalty_count == 5;
                    if cancelled {
                        self.agents[agent].customer_orders[index].status =
                            CustomerOrderStatus::Cancelled;
                    }
                    let penalty_id = format!("customer-penalty/{order_id}/{penalty_count}");
                    let settlement_id =
                        self.next_entity_id("customer-settlement", "final-settlement");
                    let settlement = CustomerSettlement {
                        id: settlement_id.clone(),
                        order_id: order_id.clone(),
                        delivered: false,
                        payment: 0,
                        penalty,
                        penalty_count,
                        cancelled,
                    };
                    self.customer_settlements[agent].push(settlement.clone());
                    self.entity_event(
                        "final-settlement",
                        "customer-order",
                        &order_id,
                        "updated",
                        customer_order_value(&self.agents[agent].customer_orders[index], self.day),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.entity_event(
                        "final-settlement",
                        "customer-settlement",
                        &settlement_id,
                        "created",
                        customer_settlement_value(&settlement),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.economic_event_with_id(
                        "customer-penalty",
                        "final-settlement",
                        &penalty_id,
                        Some(&order_id),
                        Value::Map(vec![
                            ("settlement_id".into(), Value::Text(settlement_id.clone())),
                            ("penalty_count".into(), Value::U64(u64::from(penalty_count))),
                            ("amount".into(), Value::I64(penalty)),
                        ]),
                        EventVisibility::Private(self.agents[agent].id.clone()),
                    )?;
                    self.transfer(
                        "final-settlement",
                        agent,
                        AgentTransfer::Debit("customer-market"),
                        penalty,
                        "late-penalty",
                        &penalty_id,
                    )?;
                    if cancelled {
                        self.economic_event(
                            "customer-cancellation",
                            "final-settlement",
                            Some(&order_id),
                            Value::Map(vec![
                                ("settlement_id".into(), Value::Text(settlement_id)),
                                ("penalty_count".into(), Value::U64(5)),
                            ]),
                            EventVisibility::Private(self.agents[agent].id.clone()),
                        )?;
                    }
                }
            }
        }
        self.phase_close("final-settlement")?;
        Ok(())
    }

    fn agent_index(&self, id: &str) -> Result<usize, GameError> {
        self.agents
            .iter()
            .position(|agent| agent.id == id)
            .ok_or_else(|| GameError::InvalidAction {
                agent: id.into(),
                reason: "unknown-agent",
            })
    }

    fn previous_action_for(&self, agent_id: &str) -> Option<ActionDisposition> {
        self.committed_actions.iter().rev().find_map(|value| {
            let Value::Map(fields) = value else {
                return None;
            };
            let text_field = |name: &str| {
                fields.iter().find_map(|(key, value)| {
                    (key == name)
                        .then_some(value)
                        .and_then(|value| match value {
                            Value::Text(value) => Some(value.as_str()),
                            _ => None,
                        })
                })
            };
            if text_field("agent_id") != Some(agent_id) {
                return None;
            }
            let day = fields.iter().find_map(|(key, value)| {
                (key == "day")
                    .then_some(value)
                    .and_then(|value| match value {
                        Value::U64(value) => u32::try_from(*value).ok(),
                        _ => None,
                    })
            })?;
            let action_digest = fields.iter().find_map(|(key, value)| {
                (key == "action_digest")
                    .then_some(value)
                    .and_then(|value| match value {
                        Value::Bytes(value) => value.as_slice().try_into().ok(),
                        _ => None,
                    })
            })?;
            let cause = match text_field("cause")? {
                "agent-submission" => "agent-submission",
                "missing" => "missing",
                "timeout" => "timeout",
                "process-exit" => "process-exit",
                "resource-limit" => "resource-limit",
                "transport-error" => "transport-error",
                _ => return None,
            };
            Some(ActionDisposition {
                day,
                action_digest,
                cause,
                revision: None,
            })
        })
    }

    fn state_commitment(&mut self, phase: &str) -> [u8; 32] {
        if !self.state_commitments {
            return [0; 32];
        }
        sha256_value(&self.authoritative_state_value(phase))
    }

    /// Training engine mode: skip the per-phase state commitments
    /// (observations carry zero commitments; scores, transcripts of actions
    /// and the economic summary are unchanged).
    #[must_use]
    pub fn without_state_commitments(mut self) -> Self {
        self.state_commitments = false;
        self
    }

    fn authoritative_state_value(&self, phase: &str) -> Value {
        let agents = self
            .agents
            .iter()
            .enumerate()
            .map(|(agent_index, agent)| {
                let mut supplier_orders = self
                    .supplier_lines
                    .iter()
                    .flat_map(|line| line.orders.iter())
                    .filter(|order| order.agent_id == agent.id)
                    .collect::<Vec<_>>();
                supplier_orders.sort_by(|left, right| left.id.cmp(&right.id));
                let mut customer_offers = self
                    .customer_bids
                    .iter()
                    .filter(|(_, agent_id, _)| *agent_id == agent.id)
                    .collect::<Vec<_>>();
                // Cached key: one digest per bid instead of one per comparison
                // (same order; sort_by_cached_key is stable like sort_by_key).
                customer_offers.sort_by_cached_key(|(day, agent_id, offer)| {
                    customer_bid_id(*day, agent_id, offer)
                });
                Value::Map(vec![
                    ("state_id".into(), Value::Text(agent_state_id(&agent.id))),
                    ("agent_id".into(), Value::Text(agent.id.clone())),
                    ("cash".into(), Value::I64(agent.cash)),
                    (
                        "components".into(),
                        Value::Array(inventory_values(&agent.id, "component", &agent.components)),
                    ),
                    (
                        "pcs".into(),
                        Value::Array(inventory_values(&agent.id, "pc", &agent.pcs)),
                    ),
                    (
                        "pending_components".into(),
                        Value::Array(inventory_values(
                            &agent.id,
                            "pending-component",
                            &self.pending_components[agent_index],
                        )),
                    ),
                    (
                        "pending_pcs".into(),
                        Value::Array(inventory_values(
                            &agent.id,
                            "pending-pc",
                            &self.pending_pcs[agent_index],
                        )),
                    ),
                    (
                        "production_schedule".into(),
                        Value::Array(
                            agent
                                .production_schedule
                                .iter()
                                .map(production_line_value)
                                .collect(),
                        ),
                    ),
                    (
                        "delivery_schedule".into(),
                        Value::Array(
                            agent
                                .delivery_schedule
                                .iter()
                                .map(delivery_line_value)
                                .collect(),
                        ),
                    ),
                    (
                        "production_results".into(),
                        Value::Array(
                            agent
                                .production_results
                                .iter()
                                .map(schedule_result_value)
                                .collect(),
                        ),
                    ),
                    (
                        "delivery_results".into(),
                        Value::Array(
                            agent
                                .delivery_results
                                .iter()
                                .map(schedule_result_value)
                                .collect(),
                        ),
                    ),
                    (
                        "supplier_rfqs".into(),
                        Value::Array(agent.supplier_rfqs.iter().map(supplier_rfq_value).collect()),
                    ),
                    (
                        "supplier_orders".into(),
                        Value::Array(
                            supplier_orders
                                .into_iter()
                                .map(|order| supplier_order_value(order, &order.supplier_id))
                                .collect(),
                        ),
                    ),
                    (
                        "supplier_deliveries".into(),
                        Value::Array(
                            self.supplier_deliveries[agent_index]
                                .iter()
                                .map(supplier_delivery_value)
                                .collect(),
                        ),
                    ),
                    (
                        "customer_offers".into(),
                        Value::Array(
                            customer_offers
                                .into_iter()
                                .map(|(day, agent_id, offer)| {
                                    customer_bid_value(*day, agent_id, offer)
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "customer_orders".into(),
                        Value::Array(
                            agent
                                .customer_orders
                                .iter()
                                .map(|order| customer_order_value(order, self.day))
                                .collect(),
                        ),
                    ),
                    (
                        "new_customer_order_ids".into(),
                        Value::Array(
                            self.new_customer_orders[agent_index]
                                .iter()
                                .map(|order| Value::Text(order.id.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "customer_settlements".into(),
                        Value::Array(
                            self.customer_settlements[agent_index]
                                .iter()
                                .map(customer_settlement_value)
                                .collect(),
                        ),
                    ),
                    (
                        "pending_command_rejections".into(),
                        Value::Array(
                            self.rejections[agent_index]
                                .iter()
                                .map(|rejection| {
                                    Value::Map(vec![
                                        ("agent_id".into(), Value::Text(agent.id.clone())),
                                        (
                                            "command_id".into(),
                                            Value::Text(rejection.command_id.clone()),
                                        ),
                                        ("rejection".into(), rejection_value(rejection)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "last_observation_id".into(),
                        agent
                            .last_observation_id
                            .clone()
                            .map_or(Value::Null, Value::Text),
                    ),
                    (
                        "default_history".into(),
                        Value::Array(
                            agent
                                .defaults
                                .iter()
                                .map(|(day, cause)| {
                                    Value::Map(vec![
                                        ("day".into(), Value::U64(u64::from(*day))),
                                        ("cause".into(), Value::Text(cause.text().into())),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let suppliers = self
            .supplier_lines
            .iter()
            .enumerate()
            .map(|(line_index, line)| {
                let mut pending_rfqs = self
                    .agents
                    .iter()
                    .flat_map(|agent| agent.supplier_rfqs.iter())
                    .filter(|rfq| {
                        rfq.supplier_id == line.supplier_id
                            && rfq.component_id == line.component_id
                            && rfq.due_day >= self.day
                    })
                    .collect::<Vec<_>>();
                pending_rfqs.sort_by(|left, right| left.id.cmp(&right.id));
                Value::Map(vec![
                    (
                        "line_id".into(),
                        Value::Text(supplier_line_state_id(
                            &line.supplier_id,
                            &line.component_id,
                        )),
                    ),
                    ("supplier_id".into(), Value::Text(line.supplier_id.clone())),
                    (
                        "component_id".into(),
                        Value::Text(line.component_id.clone()),
                    ),
                    (
                        "actual_capacity".into(),
                        Value::U64(u64::from(line.actual_capacity)),
                    ),
                    ("expected_capacity".into(), Value::Array(Vec::new())),
                    ("willing_capacity".into(), Value::Array(Vec::new())),
                    ("inventory".into(), Value::U64(u64::from(line.inventory))),
                    (
                        "acceptance_ordinal_next".into(),
                        Value::U64(line.next_ordinal),
                    ),
                    (
                        "orders".into(),
                        Value::Array(
                            line.orders
                                .iter()
                                .map(|order| supplier_order_value(order, &line.supplier_id))
                                .collect(),
                        ),
                    ),
                    (
                        "pending_rfqs".into(),
                        Value::Array(pending_rfqs.into_iter().map(supplier_rfq_value).collect()),
                    ),
                    (
                        "pending_offers".into(),
                        Value::Array(line.offers.iter().map(supplier_offer_value).collect()),
                    ),
                    (
                        "reputations".into(),
                        Value::Array(
                            self.agents
                                .iter()
                                .map(|agent| {
                                    let reputation = &line.reputations[&agent.id];
                                    supplier_reputation_value(&SupplierReputation {
                                        id: reputation_id(
                                            &line.supplier_id,
                                            &line.component_id,
                                            &agent.id,
                                        ),
                                        supplier_id: line.supplier_id.clone(),
                                        component_id: line.component_id.clone(),
                                        agent_id: agent.id.clone(),
                                        offered: reputation.offered,
                                        purchased: reputation.purchased,
                                        reputation_ppm: self.reputation_ppm(line_index, &agent.id),
                                    })
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let customer_orders = self
            .agents
            .iter()
            .flat_map(|agent| agent.customer_orders.iter())
            .map(|order| customer_order_value(order, self.day))
            .collect();
        let customer_offers = self
            .customer_bids
            .iter()
            .map(|(day, agent, offer)| customer_bid_value(*day, agent, offer))
            .collect();
        let staged_deliveries = self
            .staged_deliveries
            .iter()
            .map(|(agent, _, line)| {
                Value::Map(vec![
                    (
                        "agent_id".into(),
                        Value::Text(self.agents[*agent].id.clone()),
                    ),
                    ("line".into(), delivery_line_value(line)),
                ])
            })
            .collect();
        let random_streams = self
            .streams
            .byte_positions()
            .into_iter()
            .map(|(name, byte_position)| {
                Value::Map(vec![
                    ("name".into(), Value::Text(name)),
                    ("byte_position".into(), Value::U64(byte_position)),
                ])
            })
            .collect();
        let (retired_commitment, retired_count) = self.streams.retired_state();
        Value::Map(vec![
            ("game_id".into(), Value::Text(self.plan.game_id.clone())),
            (
                "day".into(),
                if phase == "setup" {
                    Value::Null
                } else {
                    Value::U64(u64::from(self.day))
                },
            ),
            ("phase".into(), Value::Text(phase.into())),
            ("agents".into(), Value::Array(agents)),
            ("supplier_lines".into(), Value::Array(suppliers)),
            (
                "customer".into(),
                Value::Map(vec![
                    (
                        "segments".into(),
                        Value::Array(self.segments.iter().map(segment_state_value).collect()),
                    ),
                    (
                        "rfqs".into(),
                        Value::Array(self.customer_rfqs.iter().map(customer_rfq_value).collect()),
                    ),
                    ("offers".into(), Value::Array(customer_offers)),
                    ("orders".into(), Value::Array(customer_orders)),
                    ("staged_deliveries".into(), Value::Array(staged_deliveries)),
                    (
                        "report_accumulator".into(),
                        market_accumulator_state_value(
                            self.market_start_day,
                            &self.product_market,
                            &self.component_market,
                            &self.capacity_market,
                        ),
                    ),
                    (
                        "daily_price_accumulator".into(),
                        Value::Array(
                            PRODUCTS
                                .iter()
                                .map(|product| {
                                    let (minimum, maximum) = self.daily_prices[product.id];
                                    Value::Map(vec![
                                        ("product_id".into(), Value::Text(product.id.into())),
                                        ("minimum".into(), minimum.map_or(Value::Null, Value::I64)),
                                        ("maximum".into(), maximum.map_or(Value::Null, Value::I64)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "pending_market_report".into(),
                        self.current_market_report
                            .as_ref()
                            .map_or(Value::Null, market_report_value),
                    ),
                ]),
            ),
            (
                "ledger".into(),
                Value::Map(vec![
                    (
                        "transactions".into(),
                        Value::Array(
                            self.ledger_transactions
                                .iter()
                                .map(ledger_transaction_value)
                                .collect(),
                        ),
                    ),
                    (
                        "accounts".into(),
                        Value::Array(
                            self.ledger_accounts
                                .iter()
                                .map(|(id, balance)| {
                                    Value::Map(vec![
                                        ("account_id".into(), Value::Text(id.clone())),
                                        ("balance".into(), Value::I64(*balance)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
            ),
            ("random_streams".into(), Value::Array(random_streams)),
            (
                "retired_random_streams_sha256".into(),
                Value::Bytes(retired_commitment.to_vec()),
            ),
            (
                "retired_random_stream_count".into(),
                Value::U64(retired_count),
            ),
            (
                "committed_actions".into(),
                Value::Array(self.committed_actions.clone()),
            ),
            (
                "termination".into(),
                Value::Map(vec![(
                    "status".into(),
                    Value::Text(
                        if phase == "terminated" || self.completed {
                            "completed"
                        } else {
                            "running"
                        }
                        .into(),
                    ),
                )]),
            ),
        ])
    }

    fn check_invariants(&self) -> Result<(), GameError> {
        if self
            .ledger_accounts
            .values()
            .map(|value| i128::from(*value))
            .sum::<i128>()
            != 0
        {
            return Err(GameError::Invariant("ledger-balance"));
        }
        if self.supplier_lines.iter().any(|line| {
            line.orders
                .iter()
                .any(|order| order.delivered > order.quantity)
        }) {
            return Err(GameError::Invariant("supplier-conservation"));
        }
        if self.supplier_lines.iter().any(|line| {
            let ordinals = line
                .orders
                .iter()
                .map(|order| order.ordinal)
                .collect::<BTreeSet<_>>();
            ordinals.len() != line.orders.len()
                || line
                    .orders
                    .iter()
                    .any(|order| order.ordinal >= line.next_ordinal)
        }) {
            return Err(GameError::Invariant("supplier-acceptance-ordinal"));
        }
        Ok(())
    }

    fn result(&mut self) -> GameResult {
        let transcript_bytes = Value::Array(self.transcript.clone()).encode();
        let encoded_item_lengths: Vec<_> = self
            .transcript
            .iter()
            .map(|item| item.encode().len())
            .collect();
        let transcript_header_len =
            transcript_bytes.len() - encoded_item_lengths.iter().sum::<usize>();
        let transcript_offsets = self
            .transcript
            .iter()
            .zip(encoded_item_lengths)
            .scan(transcript_header_len, |offset, item| {
                let current = *offset;
                *offset += item.1;
                Some(current)
            })
            .collect();
        let events_captured = matches!(self.event_output, EventOutput::Capture);
        let event_log_bytes: Vec<u8> = if events_captured {
            self.events
                .iter()
                .flat_map(|event| event.canonical_bytes.iter().copied())
                .collect()
        } else {
            Vec::new()
        };
        let event_log_sha256 = match &self.event_output {
            EventOutput::Stream { digest, .. } => digest.clone().finalize().into(),
            EventOutput::Off | EventOutput::Capture => sha256(&event_log_bytes),
        };
        let mut agent_allocation_summaries = self
            .agents
            .iter()
            .enumerate()
            .map(|(slot, agent)| {
                let supplier_units_received = self
                    .supplier_lines
                    .iter()
                    .flat_map(|line| &line.orders)
                    .filter(|order| order.agent_id == agent.id)
                    .map(|order| u64::from(order.delivered))
                    .sum::<u64>()
                    + self.retired_supplier_units[slot];
                let customer_units_awarded = agent
                    .customer_orders
                    .iter()
                    .map(|order| u64::from(order.quantity))
                    .sum();
                let customer_units_delivered = agent
                    .customer_orders
                    .iter()
                    .filter(|order| {
                        matches!(
                            order.status,
                            CustomerOrderStatus::Delivered
                                | CustomerOrderStatus::DeliveredPending { .. }
                        )
                    })
                    .map(|order| u64::from(order.quantity))
                    .sum();
                (
                    agent.id.clone(),
                    Value::Map(vec![
                        ("agent_id".into(), Value::Text(agent.id.clone())),
                        (
                            "supplier_units_received".into(),
                            Value::U64(supplier_units_received),
                        ),
                        (
                            "customer_units_awarded".into(),
                            Value::U64(customer_units_awarded),
                        ),
                        (
                            "customer_units_delivered".into(),
                            Value::U64(customer_units_delivered),
                        ),
                    ]),
                )
            })
            .collect::<Vec<_>>();
        agent_allocation_summaries.sort_by(|left, right| left.0.cmp(&right.0));
        let economic_summary_bytes = Value::Map(vec![
            (
                "schema_id".into(),
                Value::Text("tac-scm-economic-summary/2".into()),
            ),
            ("days".into(), Value::U64(u64::from(DAYS))),
            (
                "economic_kind_counts".into(),
                Value::Map(
                    self.economic_kind_counts
                        .iter()
                        .map(|(kind, count)| ((*kind).into(), Value::U64(*count)))
                        .collect(),
                ),
            ),
            (
                "ledger_transaction_count".into(),
                Value::U64(u64::try_from(self.ledger_transactions.len()).unwrap()),
            ),
            (
                "agent_allocation_summaries".into(),
                Value::Array(
                    agent_allocation_summaries
                        .into_iter()
                        .map(|(_, value)| value)
                        .collect(),
                ),
            ),
        ])
        .encode();
        let final_state_sha256 = self.state_commitment("terminated");
        let mut scores: Vec<_> = self
            .agents
            .iter()
            .map(|agent| (agent.id.clone(), agent.cash))
            .collect();
        scores.sort_by(|left, right| left.0.cmp(&right.0));
        GameResult {
            transcript_sha256: sha256(&transcript_bytes),
            transcript_bytes,
            transcript_offsets,
            event_log_sha256,
            event_log_bytes,
            event_offsets: self
                .events
                .iter()
                .scan(0_usize, |offset, event| {
                    let current = *offset;
                    *offset += event.canonical_bytes.len();
                    Some(current)
                })
                .collect(),
            event_count: self.event_count,
            events_captured,
            economic_summary_sha256: sha256(&economic_summary_bytes),
            economic_summary_bytes,
            daily_commitments: self.daily_commitments.clone(),
            final_state_sha256,
            scores,
        }
    }
}

fn valid_id(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.bytes().skip(1).all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}
fn allocation_priority(
    left: &AllocationRequest,
    left_allocated: u32,
    right: &AllocationRequest,
    right_allocated: u32,
) -> std::cmp::Ordering {
    let left_reputation = u128::from(left.reputation_ppm.max(1));
    let right_reputation = u128::from(right.reputation_ppm.max(1));
    let left_denominator =
        u128::from(left.quantity).saturating_mul(left_reputation.saturating_pow(3));
    let right_denominator =
        u128::from(right.quantity).saturating_mul(right_reputation.saturating_pow(3));
    (u128::from(left_allocated) + 1)
        .saturating_mul(right_denominator)
        .cmp(&(u128::from(right_allocated) + 1).saturating_mul(left_denominator))
        .then_with(|| (left.due_day, left.symmetry_rank).cmp(&(right.due_day, right.symmetry_rank)))
}

fn weighted_cumulative_allocation(
    requests: &[AllocationRequest],
    budgets: &[u64; DAYS as usize],
) -> Vec<u32> {
    let mut allocated = vec![0_u32; requests.len()];
    let mut by_due = [0_u64; DAYS as usize];
    loop {
        let candidate = requests
            .iter()
            .enumerate()
            .filter(|(index, request)| {
                allocated[*index] < request.quantity
                    && cumulative_feasible(&by_due, budgets, request.due_day, 1)
            })
            .min_by(|(left_index, left), (right_index, right)| {
                allocation_priority(left, allocated[*left_index], right, allocated[*right_index])
            });
        let Some((index, request)) = candidate else {
            break;
        };
        allocated[index] += 1;
        by_due[usize::try_from(request.due_day).unwrap()] += 1;
    }
    allocated
}

fn cumulative_feasible(
    by_due: &[u64; DAYS as usize],
    budgets: &[u64; DAYS as usize],
    new_due_day: u32,
    new_quantity: u64,
) -> bool {
    let mut cumulative = 0_u64;
    for day in 0..DAYS {
        cumulative += by_due[usize::try_from(day).unwrap()];
        if day >= new_due_day {
            let Some(candidate) = cumulative.checked_add(new_quantity) else {
                return false;
            };
            if candidate > budgets[usize::try_from(day).unwrap()] {
                return false;
            }
        } else if cumulative > budgets[usize::try_from(day).unwrap()] && day >= 2 {
            return false;
        }
    }
    true
}

fn maximum_cumulative_quantity(
    by_due: &[u64; DAYS as usize],
    budgets: &[u64; DAYS as usize],
    new_due_day: u32,
    requested: u64,
) -> u64 {
    let mut cumulative = 0_u64;
    let mut available = requested;
    for day in 0..DAYS {
        let Some(next) = cumulative.checked_add(by_due[usize::try_from(day).unwrap()]) else {
            return 0;
        };
        cumulative = next;
        if day >= new_due_day {
            available =
                available.min(budgets[usize::try_from(day).unwrap()].saturating_sub(cumulative));
        } else if day >= 2 && cumulative > budgets[usize::try_from(day).unwrap()] {
            return 0;
        }
    }
    available
}

#[allow(clippy::too_many_arguments)]
fn supplier_offer(
    engine_rfq: &str,
    group: &str,
    agent: &str,
    supplier: &str,
    component: &str,
    kind: SupplierOfferKind,
    quantity: u32,
    due_day: u32,
    unit_price: i64,
    valid_day: u32,
    no_offer_reason: Option<&'static str>,
    exposure: u32,
) -> SupplierOffer {
    let kind_text = match kind {
        SupplierOfferKind::OnDate => "on-date",
        SupplierOfferKind::Complete => "complete",
        SupplierOfferKind::Probe => "probe",
        SupplierOfferKind::None => "none",
    };
    let id = format!(
        "supplier-offer/{}",
        hex(&sha256(
            &Value::Array(vec![
                Value::Text(engine_rfq.into()),
                Value::U64(u64::from(valid_day - 1)),
                Value::Text(kind_text.into()),
                Value::U64(u64::from(due_day)),
                Value::U64(u64::from(quantity)),
                Value::I64(unit_price),
            ])
            .encode()
        ))
    );
    SupplierOffer {
        id,
        group_id: group.into(),
        rfq_id: engine_rfq.into(),
        agent_id: agent.into(),
        supplier_id: supplier.into(),
        component_id: component.into(),
        kind,
        quantity,
        due_day,
        unit_price,
        valid_day,
        no_offer_reason,
        exposure,
    }
}
fn visibility_value(visibility: &EventVisibility) -> Value {
    match visibility {
        EventVisibility::Public => Value::Map(vec![
            ("kind".into(), Value::Text("public".into())),
            ("audience".into(), Value::Array(Vec::new())),
        ]),
        EventVisibility::Private(agent) => Value::Map(vec![
            ("kind".into(), Value::Text("private".into())),
            (
                "audience".into(),
                Value::Array(vec![Value::Text(agent.clone())]),
            ),
        ]),
        EventVisibility::Operator => Value::Map(vec![
            ("kind".into(), Value::Text("operator".into())),
            ("audience".into(), Value::Array(Vec::new())),
        ]),
    }
}
fn market_report_value(report: &MarketReport) -> Value {
    Value::Map(vec![
        ("start_day".into(), Value::U64(u64::from(report.start_day))),
        ("end_day".into(), Value::U64(u64::from(report.end_day))),
        (
            "products".into(),
            Value::Array(
                report
                    .products
                    .iter()
                    .map(|item| {
                        Value::Map(vec![
                            ("product_id".into(), Value::Text(item.product_id.clone())),
                            ("rfq_count".into(), Value::U64(item.rfq_count)),
                            ("rfq_quantity".into(), Value::U64(item.rfq_quantity)),
                            ("order_count".into(), Value::U64(item.order_count)),
                            ("order_quantity".into(), Value::U64(item.order_quantity)),
                            (
                                "minimum_price".into(),
                                item.minimum_winning_unit_price
                                    .map_or(Value::Null, Value::I64),
                            ),
                            (
                                "maximum_price".into(),
                                item.maximum_winning_unit_price
                                    .map_or(Value::Null, Value::I64),
                            ),
                            (
                                "mean_price".into(),
                                item.mean_winning_unit_price.map_or(Value::Null, Value::I64),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "components".into(),
            Value::Array(
                report
                    .components
                    .iter()
                    .map(|item| {
                        Value::Map(vec![
                            (
                                "component_id".into(),
                                Value::Text(item.component_id.clone()),
                            ),
                            ("ordered_quantity".into(), Value::U64(item.ordered_quantity)),
                            (
                                "delivered_quantity".into(),
                                Value::U64(item.delivered_quantity),
                            ),
                            (
                                "mean_unit_price".into(),
                                item.mean_ordered_unit_price.map_or(Value::Null, Value::I64),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "supplier_lines".into(),
            Value::Array(
                report
                    .supplier_lines
                    .iter()
                    .map(|item| {
                        Value::Map(vec![
                            ("supplier_id".into(), Value::Text(item.supplier_id.clone())),
                            (
                                "component_id".into(),
                                Value::Text(item.component_id.clone()),
                            ),
                            (
                                "mean_capacity_nano".into(),
                                item.mean_capacity_nano.map_or(Value::Null, Value::U64),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn market_accumulator_state_value(
    start_day: u32,
    products: &BTreeMap<String, ProductAccumulator>,
    components: &BTreeMap<String, ComponentAccumulator>,
    supplier_lines: &BTreeMap<(String, String), CapacityAccumulator>,
) -> Value {
    Value::Map(vec![
        ("start_day".into(), Value::U64(u64::from(start_day))),
        (
            "products".into(),
            Value::Array(
                PRODUCTS
                    .iter()
                    .map(|spec| {
                        let item = &products[spec.id];
                        Value::Map(vec![
                            ("product_id".into(), Value::Text(spec.id.into())),
                            ("rfq_count".into(), Value::U64(item.rfq_count)),
                            ("rfq_quantity".into(), Value::U64(item.rfq_quantity)),
                            ("order_count".into(), Value::U64(item.order_count)),
                            ("order_quantity".into(), Value::U64(item.order_quantity)),
                            (
                                "winning_price_sum".into(),
                                Value::I64(
                                    i64::try_from(item.winning_price_sum)
                                        .expect("winning-price sum fits i64"),
                                ),
                            ),
                            (
                                "winning_price_count".into(),
                                Value::U64(item.winning_price_count),
                            ),
                            (
                                "winning_min".into(),
                                item.winning_min.map_or(Value::Null, Value::I64),
                            ),
                            (
                                "winning_max".into(),
                                item.winning_max.map_or(Value::Null, Value::I64),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "components".into(),
            Value::Array(
                COMPONENTS
                    .iter()
                    .map(|spec| {
                        let item = &components[spec.id];
                        Value::Map(vec![
                            ("component_id".into(), Value::Text(spec.id.into())),
                            ("ordered_quantity".into(), Value::U64(item.ordered_quantity)),
                            (
                                "delivered_quantity".into(),
                                Value::U64(item.delivered_quantity),
                            ),
                            (
                                "ordered_price_sum".into(),
                                Value::I64(
                                    i64::try_from(item.ordered_price_sum)
                                        .expect("ordered-price sum fits i64"),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "supplier_lines".into(),
            Value::Array(
                supplier_lines
                    .iter()
                    .map(|((supplier_id, component_id), item)| {
                        Value::Map(vec![
                            ("supplier_id".into(), Value::Text(supplier_id.clone())),
                            ("component_id".into(), Value::Text(component_id.clone())),
                            ("capacity_sum".into(), Value::U64(item.capacity_sum)),
                            ("capacity_count".into(), Value::U64(item.capacity_count)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn production_line_value(line: &ProductionLine) -> Value {
    Value::Map(vec![
        ("line_id".into(), Value::Text(line.line_id.clone())),
        ("priority".into(), Value::U64(u64::from(line.priority))),
        ("product_id".into(), Value::Text(line.product_id.clone())),
        ("quantity".into(), Value::U64(u64::from(line.quantity))),
    ])
}

fn delivery_line_value(line: &DeliveryLine) -> Value {
    Value::Map(vec![
        ("line_id".into(), Value::Text(line.line_id.clone())),
        ("priority".into(), Value::U64(u64::from(line.priority))),
        (
            "customer_order_id".into(),
            Value::Text(line.customer_order_id.clone()),
        ),
        ("quantity".into(), Value::U64(u64::from(line.quantity))),
    ])
}

fn schedule_result_id(day: u32, ordinal: u64) -> String {
    format!("schedule-result/{day}/factory-execution/{ordinal}")
}

fn agent_state_id(agent_id: &str) -> String {
    let ordinal = AGENT_IDS
        .iter()
        .position(|candidate| *candidate == agent_id)
        .expect("registered agent id");
    format!("agent-state/0/setup/{ordinal}")
}

fn supplier_line_ordinal(supplier_id: &str, component_id: &str) -> usize {
    let mut lines = COMPONENTS
        .iter()
        .flat_map(|component| {
            component
                .suppliers
                .iter()
                .map(move |supplier| (*supplier, component.id))
        })
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines
        .iter()
        .position(|candidate| *candidate == (supplier_id, component_id))
        .expect("registered supplier line")
}

fn supplier_line_state_id(supplier_id: &str, component_id: &str) -> String {
    format!(
        "supplier-line-state/0/setup/{}",
        supplier_line_ordinal(supplier_id, component_id)
    )
}

fn reputation_id(supplier_id: &str, component_id: &str, agent_id: &str) -> String {
    let agent = AGENT_IDS
        .iter()
        .position(|candidate| *candidate == agent_id)
        .expect("registered agent id");
    let ordinal = supplier_line_ordinal(supplier_id, component_id) * AGENT_IDS.len() + agent;
    format!("supplier-reputation/0/setup/{ordinal}")
}

fn supplier_rfq_id(agent_id: &str, supplier_id: &str, client_rfq_id: &str) -> String {
    format!(
        "supplier-rfq/{}",
        hex(&sha256(
            &Value::Array(vec![
                Value::Text(agent_id.into()),
                Value::Text(supplier_id.into()),
                Value::Text(client_rfq_id.into()),
            ])
            .encode(),
        ))
    )
}

fn customer_bid_id(award_day: u32, agent_id: &str, offer: &CustomerOffer) -> String {
    format!(
        "customer-bid/{}",
        hex(&sha256(
            &Value::Array(vec![
                Value::Text(agent_id.into()),
                Value::Text(offer.rfq_id.clone()),
                Value::U64(u64::from(award_day.saturating_sub(1))),
            ])
            .encode()
        ))
    )
}

fn customer_bid_value(award_day: u32, agent_id: &str, offer: &CustomerOffer) -> Value {
    Value::Map(vec![
        (
            "bid_id".into(),
            Value::Text(customer_bid_id(award_day, agent_id, offer)),
        ),
        ("agent_id".into(), Value::Text(agent_id.into())),
        ("rfq_id".into(), Value::Text(offer.rfq_id.clone())),
        (
            "bid_day".into(),
            Value::U64(u64::from(award_day.saturating_sub(1))),
        ),
        ("award_day".into(), Value::U64(u64::from(award_day))),
        ("unit_price".into(), Value::I64(offer.unit_price)),
    ])
}

fn segment_state_value(segment: &Segment) -> Value {
    Value::Map(vec![
        ("segment".into(), Value::Text(segment.id.into())),
        ("q_nano".into(), Value::U64(segment.q_nano)),
        ("trend_ppb".into(), Value::U64(segment.trend_ppb)),
        ("next_ordinal".into(), Value::U64(segment.next_ordinal)),
    ])
}

fn rejection_value(rejection: &CommandRejection) -> Value {
    Value::Map(vec![
        ("code".into(), Value::Text(rejection.code.into())),
        ("path".into(), Value::Text(rejection.field_path.clone())),
        ("detail".into(), Value::Text(rejection.detail.into())),
    ])
}

fn schedule_result_value(result: &ScheduleResult) -> Value {
    let mut fields = vec![
        ("result_id".into(), Value::Text(result.id.clone())),
        ("agent_id".into(), Value::Text(result.agent_id.clone())),
        (
            "schedule_kind".into(),
            Value::Text(result.schedule_kind.into()),
        ),
        ("line_id".into(), Value::Text(result.line_id.clone())),
        ("executed".into(), Value::Bool(result.executed)),
        ("quantity".into(), Value::U64(u64::from(result.quantity))),
    ];
    if let Some(rejection) = &result.rejection {
        fields.push(("rejection".into(), rejection_value(rejection)));
    }
    Value::Map(fields)
}

fn customer_settlement_value(settlement: &CustomerSettlement) -> Value {
    Value::Map(vec![
        ("settlement_id".into(), Value::Text(settlement.id.clone())),
        ("order_id".into(), Value::Text(settlement.order_id.clone())),
        ("delivered".into(), Value::Bool(settlement.delivered)),
        ("payment".into(), Value::I64(settlement.payment)),
        ("penalty".into(), Value::I64(settlement.penalty)),
        (
            "penalty_count".into(),
            Value::U64(u64::from(settlement.penalty_count)),
        ),
        ("cancelled".into(), Value::Bool(settlement.cancelled)),
    ])
}

fn action_disposition_value(disposition: &ActionDisposition) -> Value {
    let mut fields = vec![
        ("day".into(), Value::U64(u64::from(disposition.day))),
        (
            "action_digest".into(),
            Value::Bytes(disposition.action_digest.to_vec()),
        ),
        ("cause".into(), Value::Text(disposition.cause.into())),
    ];
    if let Some(revision) = disposition.revision {
        fields.push(("revision".into(), Value::U64(u64::from(revision))));
    }
    Value::Map(fields)
}
fn random_draw_value(draw: &RandomDraw) -> Value {
    Value::Map(vec![
        ("stream_name".into(), Value::Text(draw.stream.clone())),
        ("ordinal".into(), Value::U64(draw.ordinal)),
        ("raw_u64".into(), Value::U64(draw.raw)),
        (
            "result".into(),
            if draw.derived >= 0 {
                Value::U64(u64::try_from(draw.derived).unwrap_or(u64::MAX))
            } else {
                Value::I64(i64::try_from(draw.derived).unwrap_or(i64::MIN))
            },
        ),
    ])
}
fn random_selection_value(
    draw: &RandomDraw,
    candidates: &[String],
    selected_index: usize,
    winner: &str,
) -> Value {
    Value::Map(vec![
        ("stream_name".into(), Value::Text(draw.stream.clone())),
        ("ordinal".into(), Value::U64(draw.ordinal)),
        ("raw_u64".into(), Value::U64(draw.raw)),
        (
            "result".into(),
            Value::Map(vec![
                (
                    "candidates".into(),
                    Value::Array(candidates.iter().cloned().map(Value::Text).collect()),
                ),
                (
                    "selected_index".into(),
                    Value::U64(u64::try_from(selected_index).unwrap()),
                ),
                ("winner".into(), Value::Text(winner.into())),
            ]),
        ),
    ])
}
fn ledger_transaction_value(transaction: &LedgerTransaction) -> Value {
    Value::Map(vec![
        ("transaction_id".into(), Value::Text(transaction.id.clone())),
        ("day".into(), Value::U64(u64::from(transaction.day))),
        ("phase".into(), Value::Text(transaction.phase.into())),
        (
            "source_account".into(),
            Value::Text(transaction.source_account.clone()),
        ),
        (
            "destination_account".into(),
            Value::Text(transaction.destination_account.clone()),
        ),
        ("amount".into(), Value::I64(transaction.amount)),
        ("reason".into(), Value::Text(transaction.reason.clone())),
        ("cause_id".into(), Value::Text(transaction.cause_id.clone())),
    ])
}

fn supplier_delivery_value(delivery: &SupplierDelivery) -> Value {
    Value::Map(vec![
        ("delivery_id".into(), Value::Text(delivery.id.clone())),
        ("order_id".into(), Value::Text(delivery.order_id.clone())),
        (
            "component_id".into(),
            Value::Text(delivery.component_id.clone()),
        ),
        ("quantity".into(), Value::U64(u64::from(delivery.quantity))),
        ("unit_price".into(), Value::I64(delivery.unit_price)),
        ("invoice".into(), Value::I64(delivery.invoice)),
    ])
}

fn supplier_rfq_value(rfq: &SupplierRfq) -> Value {
    Value::Map(vec![
        ("rfq_id".into(), Value::Text(rfq.id.clone())),
        (
            "client_rfq_id".into(),
            Value::Text(rfq.client_rfq_id.clone()),
        ),
        ("agent_id".into(), Value::Text(rfq.agent_id.clone())),
        ("supplier_id".into(), Value::Text(rfq.supplier_id.clone())),
        ("component_id".into(), Value::Text(rfq.component_id.clone())),
        ("quantity".into(), Value::U64(u64::from(rfq.quantity))),
        ("due_day".into(), Value::U64(u64::from(rfq.due_day))),
        (
            "max_unit_price".into(),
            rfq.max_unit_price.map_or(Value::Null, Value::I64),
        ),
    ])
}

fn supplier_offer_value(offer: &SupplierOffer) -> Value {
    Value::Map(vec![
        ("offer_id".into(), Value::Text(offer.id.clone())),
        (
            "alternative_group_id".into(),
            Value::Text(offer.group_id.clone()),
        ),
        ("rfq_id".into(), Value::Text(offer.rfq_id.clone())),
        ("agent_id".into(), Value::Text(offer.agent_id.clone())),
        ("supplier_id".into(), Value::Text(offer.supplier_id.clone())),
        (
            "component_id".into(),
            Value::Text(offer.component_id.clone()),
        ),
        (
            "kind".into(),
            Value::Text(
                match offer.kind {
                    SupplierOfferKind::OnDate => "on-date",
                    SupplierOfferKind::Complete => "complete",
                    SupplierOfferKind::Probe => "probe",
                    SupplierOfferKind::None => "none",
                }
                .into(),
            ),
        ),
        ("quantity".into(), Value::U64(u64::from(offer.quantity))),
        ("due_day".into(), Value::U64(u64::from(offer.due_day))),
        ("unit_price".into(), Value::I64(offer.unit_price)),
        ("valid_day".into(), Value::U64(u64::from(offer.valid_day))),
        (
            "reputation_exposure".into(),
            Value::U64(u64::from(offer.exposure)),
        ),
        (
            "no_offer_reason".into(),
            offer
                .no_offer_reason
                .map_or(Value::Null, |reason| Value::Text(reason.into())),
        ),
    ])
}

fn supplier_order_value(order: &SupplierOrder, supplier_id: &str) -> Value {
    Value::Map(vec![
        ("order_id".into(), Value::Text(order.id.clone())),
        ("offer_id".into(), Value::Text(order.offer_id.clone())),
        ("agent_id".into(), Value::Text(order.agent_id.clone())),
        ("supplier_id".into(), Value::Text(supplier_id.into())),
        (
            "component_id".into(),
            Value::Text(order.component_id.clone()),
        ),
        ("quantity".into(), Value::U64(u64::from(order.quantity))),
        ("due_day".into(), Value::U64(u64::from(order.due_day))),
        ("unit_price".into(), Value::I64(order.unit_price)),
        ("acceptance_ordinal".into(), Value::U64(order.ordinal)),
        ("down_payment".into(), Value::I64(order.down_payment)),
        (
            "delivered_quantity".into(),
            Value::U64(u64::from(order.delivered)),
        ),
    ])
}

fn supplier_reputation_value(reputation: &SupplierReputation) -> Value {
    Value::Map(vec![
        ("reputation_id".into(), Value::Text(reputation.id.clone())),
        (
            "component_id".into(),
            Value::Text(reputation.component_id.clone()),
        ),
        (
            "supplier_id".into(),
            Value::Text(reputation.supplier_id.clone()),
        ),
        ("agent_id".into(), Value::Text(reputation.agent_id.clone())),
        ("offered".into(), Value::U64(reputation.offered)),
        ("purchased".into(), Value::U64(reputation.purchased)),
        (
            "reputation_ppm".into(),
            Value::U64(u64::from(reputation.reputation_ppm)),
        ),
    ])
}

fn customer_rfq_value(rfq: &CustomerRfq) -> Value {
    Value::Map(vec![
        ("rfq_id".into(), Value::Text(rfq.id.clone())),
        ("product_id".into(), Value::Text(rfq.product_id.clone())),
        ("quantity".into(), Value::U64(u64::from(rfq.quantity))),
        ("due_day".into(), Value::U64(u64::from(rfq.due_day))),
        ("unit_reserve".into(), Value::I64(rfq.reserve)),
        ("daily_penalty".into(), Value::I64(rfq.penalty)),
    ])
}

fn daily_price_report_value(day: u32, ranges: &[DailyPriceRange]) -> Value {
    Value::Map(vec![
        ("day".into(), Value::U64(u64::from(day))),
        (
            "products".into(),
            Value::Array(
                ranges
                    .iter()
                    .map(|range| {
                        Value::Map(vec![
                            ("product_id".into(), Value::Text(range.product_id.clone())),
                            (
                                "minimum".into(),
                                range.minimum.map_or(Value::Null, Value::I64),
                            ),
                            (
                                "maximum".into(),
                                range.maximum.map_or(Value::Null, Value::I64),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
fn inventory_values(
    agent_id: &str,
    inventory_kind: &str,
    items: &BTreeMap<String, u32>,
) -> Vec<Value> {
    items
        .iter()
        .map(|(id, quantity)| inventory_item_value(agent_id, inventory_kind, id, *quantity))
        .collect()
}

fn inventory_id(agent_id: &str, inventory_kind: &str, item_id: &str) -> String {
    let agent = AGENT_IDS
        .iter()
        .position(|candidate| *candidate == agent_id)
        .expect("registered agent id");
    let mut component_ids = COMPONENTS.iter().map(|item| item.id).collect::<Vec<_>>();
    component_ids.sort_unstable();
    let mut product_ids = PRODUCTS.iter().map(|item| item.id).collect::<Vec<_>>();
    product_ids.sort_unstable();
    let per_agent = 2 * (component_ids.len() + product_ids.len());
    let within_agent = match inventory_kind {
        "component" => component_ids
            .iter()
            .position(|candidate| *candidate == item_id)
            .expect("registered component"),
        "pc" => {
            component_ids.len()
                + product_ids
                    .iter()
                    .position(|candidate| *candidate == item_id)
                    .expect("registered product")
        }
        "pending-component" => {
            component_ids.len()
                + product_ids.len()
                + component_ids
                    .iter()
                    .position(|candidate| *candidate == item_id)
                    .expect("registered component")
        }
        "pending-pc" => {
            2 * component_ids.len()
                + product_ids.len()
                + product_ids
                    .iter()
                    .position(|candidate| *candidate == item_id)
                    .expect("registered product")
        }
        _ => unreachable!("registered inventory kind"),
    };
    format!(
        "inventory-item/0/setup/{}",
        agent * per_agent + within_agent
    )
}

fn inventory_item_value(
    agent_id: &str,
    inventory_kind: &str,
    item_id: &str,
    quantity: u32,
) -> Value {
    Value::Map(vec![
        (
            "inventory_id".into(),
            Value::Text(inventory_id(agent_id, inventory_kind, item_id)),
        ),
        ("agent_id".into(), Value::Text(agent_id.into())),
        ("inventory_kind".into(), Value::Text(inventory_kind.into())),
        ("item_id".into(), Value::Text(item_id.into())),
        ("quantity".into(), Value::U64(u64::from(quantity))),
    ])
}
fn customer_order_value(order: &CustomerOrder, current_day: u32) -> Value {
    Value::Map(vec![
        ("order_id".into(), Value::Text(order.id.clone())),
        ("rfq_id".into(), Value::Text(order.rfq_id.clone())),
        ("agent_id".into(), Value::Text(order.agent_id.clone())),
        ("product_id".into(), Value::Text(order.product_id.clone())),
        ("quantity".into(), Value::U64(u64::from(order.quantity))),
        ("due_day".into(), Value::U64(u64::from(order.due_day))),
        ("unit_price".into(), Value::I64(order.unit_price)),
        ("daily_penalty".into(), Value::I64(order.penalty)),
        (
            "penalty_count".into(),
            Value::U64(u64::from(order.penalty_count)),
        ),
        (
            "status".into(),
            Value::Text(
                match order.status {
                    CustomerOrderStatus::Open => "open",
                    CustomerOrderStatus::DeliveredPending { .. } => "delivered-pending-payment",
                    CustomerOrderStatus::Delivered => "delivered-paid",
                    CustomerOrderStatus::Cancelled => "cancelled",
                }
                .into(),
            ),
        ),
        (
            "payment_status".into(),
            Value::Text(
                match order.status {
                    CustomerOrderStatus::Delivered => "paid",
                    CustomerOrderStatus::DeliveredPending { .. } | CustomerOrderStatus::Open
                        if current_day >= order.due_day =>
                    {
                        "due"
                    }
                    CustomerOrderStatus::DeliveredPending { .. }
                    | CustomerOrderStatus::Open
                    | CustomerOrderStatus::Cancelled => "not-due",
                }
                .into(),
            ),
        ),
        (
            "delivery_day".into(),
            order
                .delivery_day
                .map_or(Value::Null, |day| Value::U64(u64::from(day))),
        ),
    ])
}

pub fn run_default_game(plan: &GamePlan) -> Result<GameResult, GameError> {
    let mut engine = GameEngine::new(plan.clone())?;
    run_default_engine(&mut engine)
}

pub fn run_default_game_with_events(plan: &GamePlan) -> Result<GameResult, GameError> {
    let mut engine = GameEngine::new_with_event_capture(plan.clone())?;
    run_default_engine(&mut engine)
}

fn run_default_engine(engine: &mut GameEngine) -> Result<GameResult, GameError> {
    loop {
        let observations = engine.open_day()?.to_vec();
        let inputs = observations
            .into_iter()
            .map(|observation| ActionInput::Default {
                agent_id: observation.agent_id,
                cause: DefaultCause::Missing,
            })
            .collect();
        if let Some(result) = engine.commit_day(inputs)? {
            return Ok(result);
        }
    }
}

pub fn verify_default_replay(plan: &GamePlan, expected: &GameResult) -> Result<(), GameError> {
    let actual = if expected.events_captured {
        run_default_game_with_events(plan)?
    } else {
        run_default_game(plan)?
    };
    compare_results(&actual, expected)
}

fn compare_results(actual: &GameResult, expected: &GameResult) -> Result<(), GameError> {
    if actual.transcript_bytes != expected.transcript_bytes {
        let offset = first_difference(&actual.transcript_bytes, &expected.transcript_bytes);
        return Err(GameError::ReplayMismatch {
            boundary: "accepted-action transcript",
            index: offset.map(|byte| {
                expected
                    .transcript_offsets
                    .partition_point(|item_offset| *item_offset <= byte)
                    .saturating_sub(1)
            }),
            byte_offset: offset,
        });
    }
    if actual.event_log_bytes != expected.event_log_bytes {
        let offset = first_difference(&actual.event_log_bytes, &expected.event_log_bytes);
        let index = offset.map(|byte| {
            expected
                .event_offsets
                .partition_point(|event_offset| *event_offset <= byte)
                .saturating_sub(1)
        });
        return Err(GameError::ReplayMismatch {
            boundary: "canonical event log",
            index,
            byte_offset: offset,
        });
    }
    if let Some(index) = actual
        .daily_commitments
        .iter()
        .zip(&expected.daily_commitments)
        .position(|(actual, expected)| actual != expected)
        .or_else(|| {
            (actual.daily_commitments.len() != expected.daily_commitments.len()).then_some(
                actual
                    .daily_commitments
                    .len()
                    .min(expected.daily_commitments.len()),
            )
        })
    {
        let byte_offset = actual.daily_commitments.get(index).and_then(|actual| {
            expected
                .daily_commitments
                .get(index)
                .map(|expected| first_difference(actual, expected).unwrap_or(0))
        });
        return Err(GameError::ReplayMismatch {
            boundary: "daily commitments",
            index: Some(index),
            byte_offset,
        });
    }
    if actual.final_state_sha256 != expected.final_state_sha256 {
        return Err(GameError::ReplayMismatch {
            boundary: "final state",
            index: None,
            byte_offset: first_difference(&actual.final_state_sha256, &expected.final_state_sha256),
        });
    }
    Ok(())
}

fn first_difference(actual: &[u8], expected: &[u8]) -> Option<usize> {
    actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected)
        .or_else(|| (actual.len() != expected.len()).then_some(actual.len().min(expected.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> GamePlan {
        GamePlan::materialize([7_u8; 32]).unwrap()
    }

    fn research_plan(profile: HistoricalProfile) -> GamePlan {
        GamePlan::materialize_research([7_u8; 32], profile).unwrap()
    }

    fn map_field<'a>(value: &'a Value, key: &str) -> &'a Value {
        let Value::Map(fields) = value else {
            panic!("expected map")
        };
        fields
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value))
            .unwrap_or_else(|| panic!("missing field {key}"))
    }

    fn event_contains(event: &GameEvent, text: &str) -> bool {
        event
            .canonical_bytes
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }

    #[test]
    fn configuration_identity_rejects_pre_setup_tampering() {
        let mut value = plan();
        value.profile_revision = PROFILE_REVISION + 1;
        assert!(matches!(
            GameEngine::new(value),
            Err(GameError::ConfigurationInvalid(_))
        ));
    }

    #[test]
    fn normative_zero_root_plan_identity_matches_engine_v0_4_1() {
        let plan = GamePlan::materialize([0_u8; 32]).unwrap();
        assert_eq!(plan.profile_id, PROFILE_ID);
        assert_eq!(plan.loan_rate_ppb, 105_900_000);
        assert_eq!(plan.deposit_rate_ppb, 52_950_000);
        assert_eq!(plan.storage_rate_ppb, 389_000_000);
        assert_eq!(
            hex(&plan.configuration_identity),
            "7aab9dc1c3d378cf15ed09d0987d58151326619ac97e4de643a695e8ff76d508"
        );
    }

    #[test]
    fn normative_zero_root_game_matches_engine_v0_4_1() {
        let plan = GamePlan::materialize([0_u8; 32]).unwrap();
        let result = run_default_game_with_events(&plan).unwrap();
        assert_eq!(
            hex(&result.transcript_sha256),
            "01f808934142da094ab775e292abed10383acfa00effcbce847c26f2a82377d1"
        );
        assert_eq!(
            hex(&result.final_state_sha256),
            "9fa3a3a8472028c8a081751cd13d33b14325f9862839c525fe5d00b0dc1f02d9"
        );
        assert_eq!(result.event_count, 270_201);
        assert!(result.scores.iter().all(|(_, score)| *score == 0));
    }

    #[test]
    fn research_profile_materialization_is_closed_and_validated() {
        let mut identities = BTreeSet::new();
        for profile in HistoricalProfile::ALL {
            let plan = GamePlan::materialize_research([0_u8; 32], profile).unwrap();
            assert_eq!(plan.profile_id, profile.id());
            assert_eq!(plan.profile_revision, RESEARCH_PROFILE_REVISION);
            assert_eq!(plan.historical_factors(), profile.factors());
            assert!(identities.insert(plan.configuration_identity));
            GameEngine::new(plan).unwrap();
        }
        assert_eq!(identities.len(), HistoricalProfile::ALL.len());
    }

    #[test]
    fn financial_profile_materializes_whole_percent_rates() {
        let plan =
            GamePlan::materialize_research([0_u8; 32], HistoricalProfile::FinancialCarry).unwrap();
        assert_eq!(plan.loan_rate_ppb % 10_000_000, 0);
        assert_eq!(plan.deposit_rate_ppb % 10_000_000, 0);
        assert_eq!(plan.storage_rate_ppb % 10_000_000, 0);
        assert_eq!(
            plan.deposit_rate_ppb,
            (plan.loan_rate_ppb / 20_000_000) * 10_000_000
        );
    }

    #[test]
    fn historical_finance_and_storage_use_opening_whole_credit_semantics() {
        let mut engine =
            GameEngine::new_with_event_capture(research_plan(HistoricalProfile::FinancialCarry))
                .unwrap();
        engine.agents[0].cash = -500_000;
        engine.finance_open().unwrap();
        assert_eq!(engine.agents[0].cash, -1_000_000);

        let component = &COMPONENTS[0];
        engine.agents[0]
            .components
            .insert(component.id.into(), 1_000);
        let inventory_value = u128::try_from(component.base_microcredits).unwrap() * 1_000;
        let annual_percent = engine.plan.storage_rate_ppb / 10_000_000;
        let expected = historical_storage_charge(inventory_value, annual_percent).unwrap();
        engine.historical_storage_open().unwrap();
        assert_eq!(engine.agents[0].cash, -1_000_000 - expected);
        assert!(engine.events.iter().any(|event| {
            event.phase == "historical-storage-open"
                && event.event_type == "economic-event"
                && event_contains(event, "storage-assessment")
        }));

        let cash_after_open = engine.agents[0].cash;
        engine.inventory_close().unwrap();
        assert_eq!(engine.agents[0].cash, cash_after_open);
        assert!(engine.ledger_transactions.iter().all(|transaction| {
            transaction.reason != "storage-charge" || transaction.phase == "historical-storage-open"
        }));
    }

    #[test]
    fn historical_storage_values_one_pc_as_its_component_base_sum() {
        let mut engine = GameEngine::new(research_plan(HistoricalProfile::FinancialCarry)).unwrap();
        let product = &PRODUCTS[0];
        engine.agents[0].pcs.insert(product.id.into(), 1);
        let pc_value = engine.inventory_value_microcredits(0).unwrap();
        engine.agents[0].pcs.insert(product.id.into(), 0);
        for component in product.components {
            engine.agents[0].components.insert(component.into(), 1);
        }
        assert_eq!(engine.inventory_value_microcredits(0).unwrap(), pc_value);
    }

    #[test]
    fn historical_terminal_settlement_is_sequential_retains_down_and_pays_pending_customer() {
        let mut engine = GameEngine::new_with_event_capture(research_plan(
            HistoricalProfile::TerminalSettlement,
        ))
        .unwrap();
        engine.day = 219;
        let component = engine.supplier_lines[0].component_id.clone();
        let supplier = engine.supplier_lines[0].supplier_id.clone();
        engine.supplier_lines[0].inventory = 5;
        engine.supplier_lines[0].orders = vec![
            SupplierOrder {
                id: "later-order".into(),
                offer_id: "later-offer".into(),
                agent_id: engine.agents[0].id.clone(),
                supplier_id: supplier.clone(),
                component_id: component.clone(),
                quantity: 4,
                due_day: 210,
                unit_price: 100,
                down_payment: 100,
                delivered: 0,
                ordinal: 1,
            },
            SupplierOrder {
                id: "first-order".into(),
                offer_id: "first-offer".into(),
                agent_id: engine.agents[1].id.clone(),
                supplier_id: supplier,
                component_id: component,
                quantity: 4,
                due_day: 219,
                unit_price: 100,
                down_payment: 100,
                delivered: 0,
                ordinal: 0,
            },
        ];
        let customer_agent = engine.agents[2].id.clone();
        engine.agents[2].customer_orders.push(CustomerOrder {
            id: "pending-customer-order".into(),
            rfq_id: "pending-customer-rfq".into(),
            agent_id: customer_agent,
            product_id: PRODUCTS[0].id.into(),
            quantity: 2,
            due_day: 219,
            unit_price: 100,
            penalty: 7,
            penalty_count: 0,
            status: CustomerOrderStatus::DeliveredPending { delivery_day: 219 },
            delivery_day: Some(219),
        });

        engine.final_settlement().unwrap();

        let line = &engine.supplier_lines[0];
        assert_eq!(
            line.orders
                .iter()
                .find(|order| order.ordinal == 0)
                .unwrap()
                .delivered,
            4
        );
        assert_eq!(
            line.orders
                .iter()
                .find(|order| order.ordinal == 1)
                .unwrap()
                .delivered,
            1
        );
        assert_eq!(line.inventory, 0);
        assert_eq!(line.orders[0].down_payment, 100);
        assert_eq!(line.orders[1].down_payment, 100);
        assert!(
            engine
                .ledger_transactions
                .iter()
                .all(|transaction| { transaction.reason != "supplier-refund" })
        );
        assert_eq!(engine.agents[2].cash, 200);
        assert_eq!(
            engine.agents[2].customer_orders[0].status,
            CustomerOrderStatus::Delivered
        );
    }

    #[test]
    fn order_coupled_profile_changes_only_declared_supplier_permutations() {
        let mut engine =
            GameEngine::new_with_event_capture(research_plan(HistoricalProfile::OrderCoupled))
                .unwrap();
        engine.phase_open("action-commit").unwrap();
        let before = engine.events.len();
        let candidates = vec!["second".to_owned(), "first".to_owned()];
        assert_eq!(
            engine
                .uniform_permutation(
                    "action-commit",
                    "supplier-order-acceptance",
                    "fixture",
                    candidates.clone(),
                )
                .unwrap(),
            candidates
        );
        assert_eq!(engine.events.len(), before);
        engine.phase_close("action-commit").unwrap();

        engine.phase_open("customer-settlement").unwrap();
        engine
            .uniform_permutation(
                "customer-settlement",
                "customer-award",
                "fixture",
                vec!["bid-a".into(), "bid-b".into()],
            )
            .unwrap();
        assert!(engine.events.iter().any(|event| {
            event.event_type == "random-draw" && event_contains(event, "customer-award")
        }));
    }

    #[test]
    fn supplier_market_batch_probe_changes_price_without_commitment() {
        let run = |with_probe: bool| {
            let mut engine =
                GameEngine::new(research_plan(HistoricalProfile::SupplierMarket)).unwrap();
            let observations = engine.open_day().unwrap().to_vec();
            let supplier = engine.supplier_lines[0].supplier_id.clone();
            let component = engine.supplier_lines[0].component_id.clone();
            let mut inputs = observations
                .iter()
                .map(|observation| {
                    ActionInput::Accepted(GameActionSet::empty(
                        &observation.agent_id,
                        &observation.id,
                        0,
                        1,
                    ))
                })
                .collect::<Vec<_>>();
            let ActionInput::Accepted(action) = &mut inputs[0] else {
                unreachable!()
            };
            action.supplier_rfqs.push(SupplierRfqCommand {
                command_id: "small-command".into(),
                rfq_id: "small-rfq".into(),
                supplier_id: supplier.clone(),
                component_id: component.clone(),
                quantity: 100,
                due_day: 2,
                max_unit_price: None,
            });
            if with_probe {
                let ActionInput::Accepted(probe) = &mut inputs[1] else {
                    unreachable!()
                };
                probe.supplier_rfqs.push(SupplierRfqCommand {
                    command_id: "probe-command".into(),
                    rfq_id: "probe-rfq".into(),
                    supplier_id: supplier.clone(),
                    component_id: component.clone(),
                    quantity: 0,
                    due_day: 2,
                    max_unit_price: None,
                });
            }
            engine.commit_day(inputs).unwrap();
            let line = engine
                .supplier_lines
                .iter()
                .find(|line| line.supplier_id == supplier && line.component_id == component)
                .unwrap();
            let price = line
                .offers
                .iter()
                .find(|offer| {
                    offer.agent_id == "agent-a" && offer.kind == SupplierOfferKind::OnDate
                })
                .unwrap()
                .unit_price;
            let probe_exposure = line
                .offers
                .iter()
                .find(|offer| offer.kind == SupplierOfferKind::Probe)
                .map(|offer| offer.exposure);
            (price, probe_exposure, line.orders.len())
        };

        let (without_probe, no_probe_exposure, orders_without) = run(false);
        let (with_probe, probe_exposure, orders_with) = run(true);
        assert!(with_probe > without_probe);
        assert_eq!(no_probe_exposure, None);
        assert!(probe_exposure.is_some_and(|exposure| exposure > 0));
        assert_eq!(orders_without, 0);
        assert_eq!(orders_with, 0);
    }

    #[test]
    fn supplier_market_partial_and_complete_offers_are_independent_alternatives() {
        let mut engine = GameEngine::new(research_plan(HistoricalProfile::SupplierMarket)).unwrap();
        let observations = engine.open_day().unwrap().to_vec();
        let supplier = engine.supplier_lines[0].supplier_id.clone();
        let component = engine.supplier_lines[0].component_id.clone();
        let mut inputs = observations
            .iter()
            .map(|observation| {
                ActionInput::Accepted(GameActionSet::empty(
                    &observation.agent_id,
                    &observation.id,
                    0,
                    1,
                ))
            })
            .collect::<Vec<_>>();
        let ActionInput::Accepted(action) = &mut inputs[0] else {
            unreachable!()
        };
        action.supplier_rfqs.push(SupplierRfqCommand {
            command_id: "large-command".into(),
            rfq_id: "large-rfq".into(),
            supplier_id: supplier.clone(),
            component_id: component.clone(),
            quantity: 20_000,
            due_day: 2,
            max_unit_price: None,
        });
        engine.commit_day(inputs).unwrap();
        let offers = engine
            .supplier_lines
            .iter()
            .find(|line| line.supplier_id == supplier && line.component_id == component)
            .unwrap()
            .offers
            .iter()
            .filter(|offer| offer.agent_id == "agent-a")
            .collect::<Vec<_>>();
        let on_date = offers
            .iter()
            .find(|offer| offer.kind == SupplierOfferKind::OnDate)
            .unwrap();
        let complete = offers
            .iter()
            .find(|offer| offer.kind == SupplierOfferKind::Complete)
            .unwrap();
        assert!(on_date.quantity > 0 && on_date.quantity < 20_000);
        assert_eq!(complete.quantity, 20_000);
        assert!(complete.due_day > on_date.due_day);
        assert_eq!(complete.group_id, on_date.group_id);
    }

    #[test]
    fn order_supplier_interaction_uses_validated_batch_order_without_random_draws() {
        let mut engine =
            GameEngine::new_with_event_capture(research_plan(HistoricalProfile::OrderSupplier))
                .unwrap();
        let observations = engine.open_day().unwrap().to_vec();
        let supplier = engine.supplier_lines[0].supplier_id.clone();
        let component = engine.supplier_lines[0].component_id.clone();
        let mut inputs = observations
            .iter()
            .map(|observation| {
                ActionInput::Accepted(GameActionSet::empty(
                    &observation.agent_id,
                    &observation.id,
                    0,
                    1,
                ))
            })
            .collect::<Vec<_>>();
        for (index, input) in inputs.iter_mut().take(2).enumerate() {
            let ActionInput::Accepted(action) = input else {
                unreachable!()
            };
            action.supplier_rfqs.push(SupplierRfqCommand {
                command_id: format!("joint-command-{index}"),
                rfq_id: format!("joint-rfq-{index}"),
                supplier_id: supplier.clone(),
                component_id: component.clone(),
                quantity: 1_000,
                due_day: 2,
                max_unit_price: None,
            });
        }
        engine.commit_day(inputs).unwrap();
        let line = engine
            .supplier_lines
            .iter()
            .find(|line| line.supplier_id == supplier && line.component_id == component)
            .unwrap();
        let first = line
            .offers
            .iter()
            .find(|offer| offer.agent_id == "agent-a" && offer.kind == SupplierOfferKind::OnDate)
            .unwrap();
        let second = line
            .offers
            .iter()
            .find(|offer| offer.agent_id == "agent-b" && offer.due_day == 2)
            .unwrap();
        assert!(first.quantity > 0);
        assert_eq!(second.kind, SupplierOfferKind::None);
        assert!(engine.events.iter().all(|event| {
            event.event_type != "random-draw" || !event_contains(event, "supplier-rfq-allocation")
        }));
    }

    #[test]
    fn supplier_market_complete_date_responds_to_preexisting_commitment() {
        let run = |committed_quantity: u32| {
            let mut engine =
                GameEngine::new(research_plan(HistoricalProfile::SupplierMarket)).unwrap();
            let observations = engine.open_day().unwrap().to_vec();
            let supplier = engine.supplier_lines[0].supplier_id.clone();
            let component = engine.supplier_lines[0].component_id.clone();
            if committed_quantity > 0 {
                engine.supplier_lines[0].next_ordinal = 1;
                engine.supplier_lines[0].orders.push(SupplierOrder {
                    id: "preexisting-order".into(),
                    offer_id: "preexisting-offer".into(),
                    agent_id: engine.agents[5].id.clone(),
                    supplier_id: supplier.clone(),
                    component_id: component.clone(),
                    quantity: committed_quantity,
                    due_day: 2,
                    unit_price: 1,
                    down_payment: 0,
                    delivered: 0,
                    ordinal: 0,
                });
            }
            let mut inputs = observations
                .iter()
                .map(|observation| {
                    ActionInput::Accepted(GameActionSet::empty(
                        &observation.agent_id,
                        &observation.id,
                        0,
                        1,
                    ))
                })
                .collect::<Vec<_>>();
            let ActionInput::Accepted(action) = &mut inputs[0] else {
                unreachable!()
            };
            action.supplier_rfqs.push(SupplierRfqCommand {
                command_id: "dated-command".into(),
                rfq_id: "dated-rfq".into(),
                supplier_id: supplier.clone(),
                component_id: component.clone(),
                quantity: 5_000,
                due_day: 2,
                max_unit_price: None,
            });
            engine.commit_day(inputs).unwrap();
            engine
                .supplier_lines
                .iter()
                .find(|line| line.supplier_id == supplier && line.component_id == component)
                .unwrap()
                .offers
                .iter()
                .find(|offer| {
                    offer.agent_id == "agent-a" && offer.kind == SupplierOfferKind::Complete
                })
                .unwrap()
                .due_day
        };

        let uncommitted = run(0);
        let committed = run(5_000);
        assert!(committed > uncommitted);
    }

    #[test]
    fn supplier_terminal_interaction_preserves_offer_action_and_changes_only_final_economics() {
        let mut engine =
            GameEngine::new(research_plan(HistoricalProfile::SupplierTerminal)).unwrap();
        let day_zero = engine.open_day().unwrap().to_vec();
        let supplier = engine.supplier_lines[0].supplier_id.clone();
        let component = engine.supplier_lines[0].component_id.clone();
        let mut day_zero_inputs = day_zero
            .iter()
            .map(|observation| {
                ActionInput::Accepted(GameActionSet::empty(
                    &observation.agent_id,
                    &observation.id,
                    0,
                    1,
                ))
            })
            .collect::<Vec<_>>();
        let ActionInput::Accepted(rfq_action) = &mut day_zero_inputs[0] else {
            unreachable!()
        };
        rfq_action.supplier_rfqs.push(SupplierRfqCommand {
            command_id: "terminal-rfq-command".into(),
            rfq_id: "terminal-rfq".into(),
            supplier_id: supplier.clone(),
            component_id: component.clone(),
            quantity: 100,
            due_day: 219,
            max_unit_price: None,
        });
        engine.commit_day(day_zero_inputs).unwrap();
        let offer_id = engine.supplier_lines[0]
            .offers
            .iter()
            .find(|offer| offer.agent_id == "agent-a" && offer.kind == SupplierOfferKind::OnDate)
            .unwrap()
            .id
            .clone();

        let day_one = engine.open_day().unwrap().to_vec();
        let mut day_one_inputs = day_one
            .iter()
            .map(|observation| {
                ActionInput::Accepted(GameActionSet::empty(
                    &observation.agent_id,
                    &observation.id,
                    1,
                    1,
                ))
            })
            .collect::<Vec<_>>();
        let ActionInput::Accepted(order_action) = &mut day_one_inputs[0] else {
            unreachable!()
        };
        order_action.supplier_orders.push(SupplierOrderCommand {
            command_id: "terminal-order-command".into(),
            offer_id,
        });
        engine.commit_day(day_one_inputs).unwrap();
        let line_index = engine
            .supplier_lines
            .iter()
            .position(|line| line.supplier_id == supplier && line.component_id == component)
            .unwrap();
        let original_down = engine.supplier_lines[line_index].orders[0].down_payment;
        engine.day = 219;
        engine.supplier_lines[line_index].inventory = 50;
        engine.final_settlement().unwrap();

        let order = &engine.supplier_lines[line_index].orders[0];
        assert_eq!(order.delivered, 50);
        assert_eq!(order.down_payment, original_down);
        assert_eq!(engine.supplier_lines[line_index].inventory, 0);
        assert!(
            engine
                .ledger_transactions
                .iter()
                .all(|transaction| { transaction.reason != "supplier-refund" })
        );
    }

    #[test]
    fn uniform_permutation_is_replayable_and_exchangeable_over_roots() {
        let candidates = (0..6)
            .map(|index| format!("candidate/{index}"))
            .collect::<Vec<_>>();
        let mut first_engine = GameEngine::new_with_event_capture(plan()).unwrap();
        first_engine.phase_open("action-commit").unwrap();
        let first_event_offset = first_engine.events.len();
        let first = first_engine
            .uniform_permutation(
                "action-commit",
                "test-selection",
                "fixed-context",
                candidates.clone(),
            )
            .unwrap();
        let first_event_end = first_engine.events.len();
        first_engine.phase_close("action-commit").unwrap();
        let mut replay_engine = GameEngine::new_with_event_capture(plan()).unwrap();
        replay_engine.phase_open("action-commit").unwrap();
        let replay = replay_engine
            .uniform_permutation(
                "action-commit",
                "test-selection",
                "fixed-context",
                candidates.clone(),
            )
            .unwrap();
        replay_engine.phase_close("action-commit").unwrap();
        assert_eq!(first, replay);
        assert_eq!(first_engine.events, replay_engine.events);
        assert_eq!(first.iter().cloned().collect::<BTreeSet<_>>().len(), 6);
        assert!(
            first_engine.events[first_event_offset..first_event_end]
                .iter()
                .all(|event| {
                    !event_contains(event, "agent-a")
                        && !event_contains(event, "agent-b")
                        && !event_contains(event, "agent-c")
                        && !event_contains(event, "agent-d")
                        && !event_contains(event, "agent-e")
                        && !event_contains(event, "agent-f")
                })
        );
        let event_log = first_engine
            .events
            .iter()
            .flat_map(|event| event.canonical_bytes.iter().copied())
            .collect::<Vec<_>>();
        crate::conformance::validate_event_log(&event_log).unwrap();

        let mut first_rank_counts = [0_u32; 6];
        for root in 0_u64..600 {
            let mut seed = [0_u8; 32];
            seed[..8].copy_from_slice(&root.to_be_bytes());
            let mut engine = GameEngine::new(GamePlan::materialize(seed).unwrap()).unwrap();
            let permutation = engine
                .uniform_permutation(
                    "action-commit",
                    "test-selection",
                    "fixed-context",
                    candidates.clone(),
                )
                .unwrap();
            let index = permutation[0]
                .strip_prefix("candidate/")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            first_rank_counts[index] += 1;
        }
        let minimum = *first_rank_counts.iter().min().unwrap();
        let maximum = *first_rank_counts.iter().max().unwrap();
        assert!(minimum >= 60, "first-rank counts: {first_rank_counts:?}");
        assert!(maximum <= 140, "first-rank counts: {first_rank_counts:?}");
    }

    #[test]
    fn duplicate_customer_offers_are_all_rejected() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let rfq_id = day0[0].customer_rfqs[0].id.clone();
        let mut action = GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1);
        action.customer_offers = vec![
            CustomerOffer {
                command_id: "duplicate-a".into(),
                rfq_id: rfq_id.clone(),
                unit_price: 1,
            },
            CustomerOffer {
                command_id: "duplicate-b".into(),
                rfq_id,
                unit_price: 1,
            },
        ];
        engine.commit_day(defaults_except(&day0, action)).unwrap();
        let day1 = engine.open_day().unwrap();
        assert_eq!(day1[0].rejections.len(), 2);
        assert!(
            day1[0]
                .rejections
                .iter()
                .all(|rejection| rejection.code == "cardinality")
        );
        assert!(engine.customer_bids.is_empty());
    }

    #[test]
    fn awarded_customer_rfq_is_removed_and_cross_day_reoffer_is_rejected() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let rfq = day0[0].customer_rfqs[0].clone();
        let mut first = GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1);
        first.customer_offers.push(CustomerOffer {
            command_id: "initial-bid".into(),
            rfq_id: rfq.id.clone(),
            unit_price: 1,
        });
        engine.commit_day(defaults_except(&day0, first)).unwrap();
        assert!(engine.latest_rejections()[0].is_empty());
        let day1 = engine.open_day().unwrap().to_vec();
        assert_eq!(day1[0].new_customer_orders.len(), 1);
        let order = day1[0].new_customer_orders[0].clone();
        assert_eq!(order.rfq_id, rfq.id);
        assert!(engine.customer_rfqs.iter().all(|row| row.id != rfq.id));
        assert_customer_rfq_removed_once(&engine, &rfq, 1);
        validate_captured_events(&engine);

        let mut repeated = GameActionSet::empty(&day1[0].agent_id, &day1[0].id, 1, 1);
        repeated.customer_offers.push(CustomerOffer {
            command_id: "remembered-bid".into(),
            rfq_id: rfq.id.clone(),
            unit_price: 1,
        });
        engine.commit_day(defaults_except(&day1, repeated)).unwrap();
        let rejections = &engine.latest_rejections()[0];
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].command_id, "remembered-bid");
        assert_eq!(rejections[0].code, "unknown-reference");
        assert!(
            engine
                .customer_bids
                .iter()
                .all(|(_, _, offer)| offer.rfq_id != rfq.id)
        );
        let day2 = engine.open_day().unwrap().to_vec();
        assert!(day2[0].new_customer_orders.is_empty());
        assert_eq!(
            day2[0]
                .customer_orders
                .iter()
                .filter(|row| row.id == order.id)
                .count(),
            1
        );
        assert_customer_rfq_removed_once(&engine, &rfq, 1);
        validate_captured_events(&engine);
    }

    #[test]
    fn unawarded_customer_rfq_accepts_a_late_first_bid() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let rfq = day0[0].customer_rfqs[0].clone();
        let no_bid = GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1);
        engine.commit_day(defaults_except(&day0, no_bid)).unwrap();
        let day1 = engine.open_day().unwrap().to_vec();
        assert!(day1[0].customer_rfqs.iter().all(|row| row.id != rfq.id));
        assert!(day1[0].new_customer_orders.is_empty());
        assert!(engine.customer_rfqs.iter().any(|row| row.id == rfq.id));
        let mut delayed = GameActionSet::empty(&day1[0].agent_id, &day1[0].id, 1, 1);
        delayed.customer_offers.push(CustomerOffer {
            command_id: "late-first-bid".into(),
            rfq_id: rfq.id.clone(),
            unit_price: 1,
        });
        engine.commit_day(defaults_except(&day1, delayed)).unwrap();
        assert!(engine.latest_rejections()[0].is_empty());
        let day2 = engine.open_day().unwrap().to_vec();
        assert_eq!(day2[0].new_customer_orders.len(), 1);
        assert_eq!(day2[0].new_customer_orders[0].rfq_id, rfq.id);
        assert!(engine.customer_rfqs.iter().all(|row| row.id != rfq.id));
        assert_customer_rfq_removed_once(&engine, &rfq, 2);
        validate_captured_events(&engine);
    }

    #[test]
    fn unawarded_customer_rfq_expires_once_and_rejects_later_bids() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let mut observations = engine.open_day().unwrap().to_vec();
        let rfq = observations[0].customer_rfqs[0].clone();
        for day in 0..=rfq.due_day {
            assert!(engine.customer_rfqs.iter().any(|row| row.id == rfq.id));
            let no_bid =
                GameActionSet::empty(&observations[0].agent_id, &observations[0].id, day, 1);
            engine
                .commit_day(defaults_except(&observations, no_bid))
                .unwrap();
            observations = engine.open_day().unwrap().to_vec();
        }
        assert!(engine.customer_rfqs.iter().all(|row| row.id != rfq.id));
        assert_customer_rfq_removed_once(&engine, &rfq, rfq.due_day + 1);
        let mut expired_bid = GameActionSet::empty(
            &observations[0].agent_id,
            &observations[0].id,
            rfq.due_day + 1,
            1,
        );
        expired_bid.customer_offers.push(CustomerOffer {
            command_id: "expired-bid".into(),
            rfq_id: rfq.id.clone(),
            unit_price: 1,
        });
        engine
            .commit_day(defaults_except(&observations, expired_bid))
            .unwrap();
        assert_eq!(engine.latest_rejections()[0].len(), 1);
        assert_eq!(engine.latest_rejections()[0][0].code, "unknown-reference");
        let next = engine.open_day().unwrap();
        assert!(next[0].customer_orders.is_empty());
        assert_customer_rfq_removed_once(&engine, &rfq, rfq.due_day + 1);
        validate_captured_events(&engine);
    }

    fn assert_customer_rfq_removed_once(engine: &GameEngine, rfq: &CustomerRfq, day: u32) {
        let removals = engine
            .events
            .iter()
            .filter(|event| {
                if event.event_type != "entity-changed" {
                    return false;
                }
                let (decoded, consumed) = crate::cbor::decode_one(&event.canonical_bytes).unwrap();
                assert_eq!(consumed, event.canonical_bytes.len());
                let payload = map_field(&decoded, "payload");
                if map_field(payload, "entity_type") != &Value::Text("customer-rfq".into())
                    || map_field(payload, "entity_id") != &Value::Text(rfq.id.clone())
                    || map_field(payload, "operation") != &Value::Text("removed".into())
                {
                    return false;
                }
                assert_eq!(event.day, Some(day));
                assert_eq!(event.phase, "customer-settlement");
                assert_eq!(event.visibility, EventVisibility::Public);
                assert_eq!(
                    map_field(payload, "value"),
                    &Value::Bytes(customer_rfq_value(rfq).encode())
                );
                true
            })
            .count();
        assert_eq!(removals, 1);
    }

    fn validate_captured_events(engine: &GameEngine) {
        let log = engine
            .events
            .iter()
            .flat_map(|event| event.canonical_bytes.iter().copied())
            .collect::<Vec<u8>>();
        crate::conformance::validate_event_log(&log).unwrap();
    }

    #[test]
    fn customer_award_uses_the_seeded_uniform_bid_selection() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let rfq = day0[0].customer_rfqs[0].clone();
        let mut bid_ids = Vec::new();
        let actions = day0
            .iter()
            .enumerate()
            .map(|(index, observation)| {
                let offer = CustomerOffer {
                    command_id: format!("bid-{index}"),
                    rfq_id: rfq.id.clone(),
                    unit_price: rfq.reserve,
                };
                bid_ids.push((
                    customer_bid_id(1, &observation.agent_id, &offer),
                    observation.agent_id.clone(),
                ));
                let mut action = GameActionSet::empty(&observation.agent_id, &observation.id, 0, 1);
                action.customer_offers.push(offer);
                ActionInput::Accepted(action)
            })
            .collect();
        engine.commit_day(actions).unwrap();
        bid_ids.sort();
        let mut streams = NamedStreams::new([7_u8; 32], PROFILE_ID);
        let (selected, _) = streams
            .uniform_u64(
                &format!(
                    "symmetry/1/customer-settlement/customer-award/{}/rank/0",
                    rfq.id
                ),
                0,
                5,
            )
            .unwrap();
        let expected_winner = bid_ids[usize::try_from(selected).unwrap()].1.clone();
        let day1 = engine.open_day().unwrap();
        let winners = day1
            .iter()
            .filter(|observation| {
                observation
                    .new_customer_orders
                    .iter()
                    .any(|order| order.rfq_id == rfq.id)
            })
            .map(|observation| observation.agent_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(winners, vec![expected_winner]);
        assert!(engine.events.iter().any(|event| {
            event.event_type == "random-draw"
                && event_contains(event, "customer-award")
                && event_contains(event, "candidates")
        }));
    }

    #[test]
    fn cumulative_supplier_allocation_uses_symmetry_rank_for_exact_ties() {
        let mut requests = vec![
            AllocationRequest {
                id: "supplier-rfq/a".into(),
                quantity: 1,
                reputation_ppm: 1_000_000,
                due_day: 2,
                symmetry_rank: 0,
            },
            AllocationRequest {
                id: "supplier-rfq/b".into(),
                quantity: 1,
                reputation_ppm: 1_000_000,
                due_day: 2,
                symmetry_rank: 1,
            },
        ];
        let mut budgets = [0_u64; DAYS as usize];
        budgets[2..].fill(1);
        assert_eq!(weighted_cumulative_allocation(&requests, &budgets), [1, 0]);
        requests[0].symmetry_rank = 1;
        requests[1].symmetry_rank = 0;
        assert_eq!(weighted_cumulative_allocation(&requests, &budgets), [0, 1]);
    }

    #[test]
    fn observation_identity_binds_pre_observation_state() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let observations = engine.open_day().unwrap();
        assert_eq!(observations.len(), 6);
        assert!(observations.iter().all(|observation| observation.state_commitment == observations[0].state_commitment));
        assert!(observations.iter().all(|observation| {
            observation.id
                == format!(
                    "observation/{}",
                    hex(&sha256(
                        &Value::Array(vec![
                            Value::U64(0),
                            Value::Text(observation.agent_id.clone()),
                            Value::Bytes(observation.state_commitment.to_vec()),
                        ])
                        .encode(),
                    ))
                )
        }));
    }

    #[test]
    fn authoritative_state_is_closed_and_major_families_are_sensitive() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let state = engine.authoritative_state_value("setup");
        let Value::Map(fields) = &state else {
            panic!("authoritative state must be a map")
        };
        assert_eq!(
            fields
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "game_id",
                "day",
                "phase",
                "agents",
                "supplier_lines",
                "customer",
                "ledger",
                "random_streams",
                "retired_random_streams_sha256",
                "retired_random_stream_count",
                "committed_actions",
                "termination",
            ]
        );
        let Value::Array(agents) = map_field(&state, "agents") else {
            panic!("agents must be an array")
        };
        assert_eq!(agents.len(), 6);
        assert_eq!(
            match map_field(&agents[0], "components") {
                Value::Array(items) => items.len(),
                _ => 0,
            },
            COMPONENTS.len()
        );
        assert_eq!(
            match map_field(&agents[0], "pcs") {
                Value::Array(items) => items.len(),
                _ => 0,
            },
            PRODUCTS.len()
        );
        let mut previous = engine.state_commitment("setup");
        engine.agents[0].cash = 1;
        let changed = engine.state_commitment("setup");
        assert_ne!(previous, changed, "agent state must affect commitment");
        previous = changed;
        engine.supplier_lines[0].inventory = 1;
        let changed = engine.state_commitment("setup");
        assert_ne!(previous, changed, "supplier state must affect commitment");
        previous = changed;
        engine.segments[0].q_nano += 1;
        let changed = engine.state_commitment("setup");
        assert_ne!(previous, changed, "customer state must affect commitment");
        previous = changed;
        *engine.ledger_accounts.get_mut("customer-market").unwrap() = 1;
        let changed = engine.state_commitment("setup");
        assert_ne!(previous, changed, "ledger state must affect commitment");
        previous = changed;
        engine.committed_actions.push(Value::Map(vec![
            ("day".into(), Value::U64(0)),
            ("agent_id".into(), Value::Text("agent-0".into())),
            ("action_digest".into(), Value::Bytes(vec![0; 32])),
            ("cause".into(), Value::Text("missing".into())),
        ]));
        let changed = engine.state_commitment("setup");
        assert_ne!(
            previous, changed,
            "committed actions must affect commitment"
        );
        previous = changed;
        engine.completed = true;
        assert_ne!(
            previous,
            engine.state_commitment("setup"),
            "termination must affect commitment"
        );
    }

    #[test]
    fn payment_status_is_day_relative_and_cancelled_orders_are_not_due() {
        let base = CustomerOrder {
            id: "customer-order/customer-rfq/0/low/0".into(),
            rfq_id: "customer-rfq/0/low/0".into(),
            agent_id: "agent-0".into(),
            product_id: "product-0".into(),
            quantity: 1,
            due_day: 5,
            unit_price: 1,
            penalty: 1,
            penalty_count: 0,
            status: CustomerOrderStatus::Open,
            delivery_day: None,
        };
        assert_eq!(
            map_field(&customer_order_value(&base, 4), "payment_status"),
            &Value::Text("not-due".into())
        );
        assert_eq!(
            map_field(&customer_order_value(&base, 5), "payment_status"),
            &Value::Text("due".into())
        );
        let mut cancelled = base;
        cancelled.status = CustomerOrderStatus::Cancelled;
        assert_eq!(
            map_field(&customer_order_value(&cancelled, 10), "payment_status"),
            &Value::Text("not-due".into())
        );
    }

    #[test]
    fn terminal_customer_order_is_observed_once_then_removed() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let order = CustomerOrder {
            id: "customer-order/customer-rfq/0/low/999".into(),
            rfq_id: "customer-rfq/0/low/999".into(),
            agent_id: engine.agents[0].id.clone(),
            product_id: PRODUCTS[0].id.into(),
            quantity: 1,
            due_day: 0,
            unit_price: 1,
            penalty: 1,
            penalty_count: 5,
            status: CustomerOrderStatus::Cancelled,
            delivery_day: None,
        };
        engine
            .entity_event(
                "setup",
                "customer-order",
                &order.id,
                "created",
                customer_order_value(&order, 0),
                EventVisibility::Private(order.agent_id.clone()),
            )
            .unwrap();
        engine.agents[0].customer_orders.push(order.clone());
        engine.observation_phase().unwrap();
        assert!(
            engine.observations[0]
                .customer_orders
                .iter()
                .any(|observed| observed.id == order.id)
        );
        assert!(engine.agents[0].customer_orders.is_empty());
        assert!(engine.events.iter().any(|event| {
            event.phase == "observation"
                && event.event_type == "entity-changed"
                && event_contains(event, &order.id)
                && event_contains(event, "removed")
        }));
        engine.observation_phase().unwrap();
        assert!(engine.observations[0].customer_orders.is_empty());
    }

    #[test]
    fn early_delivery_survives_observation_cleanup_until_due_day_payment() {
        let mut engine = GameEngine::new(plan()).unwrap();
        engine.day = 1;
        let order = CustomerOrder {
            id: "customer-order/customer-rfq/0/low/998".into(),
            rfq_id: "customer-rfq/0/low/998".into(),
            agent_id: engine.agents[0].id.clone(),
            product_id: PRODUCTS[0].id.into(),
            quantity: 2,
            due_day: 3,
            unit_price: 100,
            penalty: 17,
            penalty_count: 0,
            status: CustomerOrderStatus::DeliveredPending { delivery_day: 1 },
            delivery_day: Some(1),
        };
        engine.agents[0].customer_orders.push(order.clone());

        engine.observation_phase().unwrap();
        assert_eq!(engine.agents[0].customer_orders, vec![order.clone()]);

        engine.day = 3;
        engine.customer_settlement().unwrap();
        assert_eq!(engine.agents[0].cash, 200);
        assert_eq!(
            engine.agents[0].customer_orders[0].status,
            CustomerOrderStatus::Delivered
        );
        assert_eq!(engine.customer_settlements[0].len(), 1);
        assert_eq!(engine.customer_settlements[0][0].payment, 200);
        assert_eq!(engine.customer_settlements[0][0].penalty, 0);
        assert_eq!(
            engine
                .ledger_transactions
                .iter()
                .filter(|transaction| transaction.reason == "customer-payment")
                .count(),
            1
        );
        assert!(
            engine
                .ledger_transactions
                .iter()
                .all(|transaction| transaction.reason != "late-penalty")
        );

        engine.observation_phase().unwrap();
        assert!(engine.agents[0].customer_orders.is_empty());
    }

    #[test]
    fn operator_summary_precedes_observation_close() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        engine.open_day().unwrap();
        let summary = engine
            .events
            .iter()
            .position(|event| {
                event.phase == "observation" && event.event_type == "operator-summary"
            })
            .unwrap();
        let close = engine
            .events
            .iter()
            .position(|event| event.phase == "observation" && event.event_type == "phase-closed")
            .unwrap();
        assert!(summary < close);
    }

    #[test]
    fn observation_operator_summary_preserves_daily_results_before_cleanup() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let agent_id = engine.agents[0].id.clone();
        engine.agents[0].production_results.push(ScheduleResult {
            id: "schedule-result/test-production".into(),
            agent_id,
            schedule_kind: "production",
            line_id: "production/test-line".into(),
            executed: true,
            quantity: 17,
            rejection: None,
        });

        engine.observation_phase().unwrap();

        let summary = engine
            .events
            .iter()
            .position(|event| {
                event.phase == "observation"
                    && event.event_type == "operator-summary"
                    && event_contains(event, "schedule-result/test-production")
            })
            .expect("observation summary must contain the daily production result");
        let removal = engine
            .events
            .iter()
            .position(|event| {
                event.phase == "observation"
                    && event.event_type == "entity-changed"
                    && event_contains(event, "schedule-result/test-production")
                    && event_contains(event, "removed")
            })
            .expect("daily production result must be removed after observation");
        assert!(summary < removal);
        assert!(engine.agents[0].production_results.is_empty());
    }

    #[test]
    fn structural_action_failure_is_atomic() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let observations = engine.open_day().unwrap().to_vec();
        let inputs = observations
            .iter()
            .map(|observation| {
                ActionInput::Accepted(GameActionSet::empty(&observation.agent_id, "wrong", 0, 1))
            })
            .collect();
        assert!(matches!(
            engine.commit_day(inputs),
            Err(GameError::InvalidAction { .. })
        ));
        assert_eq!(engine.transcript.len(), 0);
    }

    #[test]
    fn complete_game_is_byte_repeatable_and_replayable() {
        let plan = plan();
        let first = run_default_game_with_events(&plan).unwrap();
        let second = run_default_game_with_events(&plan).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.daily_commitments.len(), 220);
        assert_eq!(first.scores.len(), 6);
        let (economic_summary, consumed) =
            crate::cbor::decode_one(&first.economic_summary_bytes).unwrap();
        assert_eq!(consumed, first.economic_summary_bytes.len());
        let Value::Map(economic_fields) = economic_summary else {
            panic!("economic summary must be a map");
        };
        assert!(economic_fields.contains(&(
            "schema_id".into(),
            Value::Text("tac-scm-economic-summary/2".into())
        )));
        let allocations = economic_fields
            .iter()
            .find_map(|(key, value)| (key == "agent_allocation_summaries").then_some(value))
            .expect("economic summary must expose allocation evidence");
        let Value::Array(allocations) = allocations else {
            panic!("allocation evidence must be an array");
        };
        assert_eq!(allocations.len(), 6);
        assert!(
            allocations
                .iter()
                .all(|value| matches!(value, Value::Map(fields)
            if fields.iter().any(|(key, _)| key == "supplier_units_received")
                && fields.iter().any(|(key, _)| key == "customer_units_awarded")
                && fields.iter().any(|(key, _)| key == "customer_units_delivered")))
        );
        compare_results(&second, &first).unwrap();
        let report = crate::conformance::validate_event_log(&first.event_log_bytes).unwrap();
        assert!(report.event_count > 1_000);
        assert!(report.entity_id_count > 100);
        assert!(report.economic_id_count > 100);
        let mut mutated = first.event_log_bytes.clone();
        let last = mutated.len() - 1;
        mutated[last] ^= 1;
        assert!(crate::conformance::validate_event_log(&mutated).is_err());
    }

    #[test]
    fn event_off_capture_and_stream_preserve_game_semantics() {
        let plan = plan();
        let mut off_engine = GameEngine::new(plan.clone()).unwrap();
        let mut capture_engine = GameEngine::new_with_event_capture(plan.clone()).unwrap();
        for _ in 0..3 {
            let off_observations = off_engine.open_day().unwrap().to_vec();
            let captured_observations = capture_engine.open_day().unwrap().to_vec();
            assert_eq!(off_observations, captured_observations);
            let inputs: Vec<_> = off_observations
                .iter()
                .map(|observation| ActionInput::Default {
                    agent_id: observation.agent_id.clone(),
                    cause: DefaultCause::Missing,
                })
                .collect();
            assert_eq!(
                off_engine.commit_day(inputs.clone()).unwrap(),
                capture_engine.commit_day(inputs).unwrap()
            );
        }
        let off = run_default_game(&plan).unwrap();
        let captured = run_default_game_with_events(&plan).unwrap();
        assert!(!off.events_captured);
        assert!(off.event_log_bytes.is_empty());
        assert!(captured.events_captured);
        assert!(!captured.event_log_bytes.is_empty());
        assert_eq!(off.event_count, captured.event_count);
        assert_eq!(off.transcript_bytes, captured.transcript_bytes);
        assert_eq!(off.economic_summary_bytes, captured.economic_summary_bytes);
        assert_eq!(off.daily_commitments, captured.daily_commitments);
        assert_eq!(off.final_state_sha256, captured.final_state_sha256);
        assert_eq!(off.scores, captured.scores);

        let streamed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&streamed);
        let mut engine = GameEngine::new_with_event_stream(plan, move |bytes| {
            sink.lock().unwrap().extend_from_slice(bytes);
        })
        .unwrap();
        let streamed_result = run_default_engine(&mut engine).unwrap();
        let streamed_bytes = streamed.lock().unwrap().clone();
        assert!(!streamed_result.events_captured);
        assert!(streamed_result.event_log_bytes.is_empty());
        assert_eq!(streamed_result.event_count, captured.event_count);
        assert_eq!(streamed_result.event_log_sha256, captured.event_log_sha256);
        assert_eq!(streamed_result.transcript_bytes, captured.transcript_bytes);
        assert_eq!(
            streamed_result.economic_summary_bytes,
            captured.economic_summary_bytes
        );
        assert_eq!(
            streamed_result.daily_commitments,
            captured.daily_commitments
        );
        assert_eq!(
            streamed_result.final_state_sha256,
            captured.final_state_sha256
        );
        assert_eq!(streamed_result.scores, captured.scores);
        assert_eq!(streamed_bytes, captured.event_log_bytes);
        crate::conformance::validate_event_log(&streamed_bytes).unwrap();
    }

    #[test]
    fn replay_rejects_each_authoritative_boundary() {
        let plan = plan();
        let result = run_default_game_with_events(&plan).unwrap();
        let mut transcript = result.clone();
        transcript.transcript_bytes[0] ^= 1;
        assert!(matches!(
            compare_results(&result, &transcript),
            Err(GameError::ReplayMismatch {
                boundary: "accepted-action transcript",
                byte_offset: Some(0),
                ..
            })
        ));
        let mut events = result.clone();
        events.event_log_bytes[0] ^= 1;
        assert!(matches!(
            compare_results(&result, &events),
            Err(GameError::ReplayMismatch {
                boundary: "canonical event log",
                index: Some(0),
                byte_offset: Some(0),
            })
        ));
        let mut commitments = result.clone();
        commitments.daily_commitments[10][0] ^= 1;
        assert!(matches!(
            compare_results(&result, &commitments),
            Err(GameError::ReplayMismatch {
                boundary: "daily commitments",
                index: Some(10),
                byte_offset: Some(0),
            })
        ));
        let mut state = result.clone();
        state.final_state_sha256[0] ^= 1;
        assert!(matches!(
            compare_results(&result, &state),
            Err(GameError::ReplayMismatch {
                boundary: "final state",
                byte_offset: Some(0),
                ..
            })
        ));
    }

    #[test]
    fn default_does_not_disable_recovered_agent() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let defaults = day0
            .into_iter()
            .map(|observation| ActionInput::Default {
                agent_id: observation.agent_id,
                cause: DefaultCause::Timeout,
            })
            .collect();
        assert!(engine.commit_day(defaults).unwrap().is_none());
        let day1 = engine.open_day().unwrap().to_vec();
        let actions = day1
            .into_iter()
            .map(|observation| {
                ActionInput::Accepted(GameActionSet::empty(
                    &observation.agent_id,
                    &observation.id,
                    1,
                    1,
                ))
            })
            .collect();
        assert!(engine.commit_day(actions).unwrap().is_none());
        assert_eq!(engine.agents[0].defaults, vec![(0, DefaultCause::Timeout)]);
    }

    fn defaults_except(observations: &[Observation], accepted: GameActionSet) -> Vec<ActionInput> {
        observations
            .iter()
            .map(|observation| {
                if observation.agent_id == accepted.agent_id {
                    ActionInput::Accepted(accepted.clone())
                } else {
                    ActionInput::Default {
                        agent_id: observation.agent_id.clone(),
                        cause: DefaultCause::Missing,
                    }
                }
            })
            .collect()
    }

    #[test]
    fn supplier_complete_alternative_and_reputation_expiry() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let mut action = GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1);
        action.supplier_rfqs.push(SupplierRfqCommand {
            command_id: "request".into(),
            rfq_id: "rfq-a".into(),
            supplier_id: "pintel".into(),
            component_id: "cpu-pintel-2".into(),
            quantity: 1_000,
            due_day: 2,
            max_unit_price: None,
        });
        engine.commit_day(defaults_except(&day0, action)).unwrap();
        let day1 = engine.open_day().unwrap().to_vec();
        let offers = &day1[0].supplier_offers;
        assert_eq!(offers.len(), 2);
        assert!(
            offers
                .iter()
                .any(|offer| offer.kind == SupplierOfferKind::OnDate)
        );
        let complete = offers
            .iter()
            .find(|offer| offer.kind == SupplierOfferKind::Complete)
            .unwrap();
        assert_eq!(complete.quantity, 1_000);
        assert!(complete.due_day > 2);

        let mut order = GameActionSet::empty(&day1[0].agent_id, &day1[0].id, 1, 1);
        order.supplier_orders.push(SupplierOrderCommand {
            command_id: "order".into(),
            offer_id: complete.id.clone(),
        });
        engine.commit_day(defaults_except(&day1, order)).unwrap();
        let line = engine
            .supplier_lines
            .iter()
            .find(|line| line.supplier_id == "pintel" && line.component_id == "cpu-pintel-2")
            .unwrap();
        let reputation = &line.reputations[&engine.agents[0].id];
        assert_eq!(reputation.offered, 1_000);
        assert_eq!(reputation.purchased, 1_000);
    }

    #[test]
    fn simultaneous_supplier_orders_use_the_seeded_uniform_acceptance_order() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let day0_actions = day0
            .iter()
            .enumerate()
            .map(|(index, observation)| {
                if index < 2 {
                    let mut action =
                        GameActionSet::empty(&observation.agent_id, &observation.id, 0, 1);
                    action.supplier_rfqs.push(SupplierRfqCommand {
                        command_id: format!("request-{index}"),
                        rfq_id: format!("rfq-{index}"),
                        supplier_id: "pintel".into(),
                        component_id: "cpu-pintel-2".into(),
                        quantity: 1,
                        due_day: 2,
                        max_unit_price: None,
                    });
                    ActionInput::Accepted(action)
                } else {
                    ActionInput::Default {
                        agent_id: observation.agent_id.clone(),
                        cause: DefaultCause::Missing,
                    }
                }
            })
            .collect();
        engine.commit_day(day0_actions).unwrap();
        let day1 = engine.open_day().unwrap().to_vec();
        let mut candidate_offers = Vec::new();
        let day1_actions = day1
            .iter()
            .enumerate()
            .map(|(index, observation)| {
                if index < 2 {
                    let offer = observation
                        .supplier_offers
                        .iter()
                        .find(|offer| {
                            offer.supplier_id == "pintel"
                                && offer.component_id == "cpu-pintel-2"
                                && offer.kind == SupplierOfferKind::OnDate
                        })
                        .unwrap();
                    candidate_offers.push(offer.id.clone());
                    let mut action =
                        GameActionSet::empty(&observation.agent_id, &observation.id, 1, 1);
                    action.supplier_orders.push(SupplierOrderCommand {
                        command_id: format!("order-{index}"),
                        offer_id: offer.id.clone(),
                    });
                    ActionInput::Accepted(action)
                } else {
                    ActionInput::Default {
                        agent_id: observation.agent_id.clone(),
                        cause: DefaultCause::Missing,
                    }
                }
            })
            .collect();
        candidate_offers.sort();
        let mut streams = NamedStreams::new([7_u8; 32], PROFILE_ID);
        let (selected, _) = streams
            .uniform_u64(
                "symmetry/1/action-commit/supplier-order-acceptance/pintel/cpu-pintel-2/rank/0",
                0,
                1,
            )
            .unwrap();
        let expected_first = candidate_offers[usize::try_from(selected).unwrap()].clone();
        engine.commit_day(day1_actions).unwrap();
        let line = engine
            .supplier_lines
            .iter()
            .find(|line| line.supplier_id == "pintel" && line.component_id == "cpu-pintel-2")
            .unwrap();
        assert_eq!(line.orders.len(), 2);
        assert_eq!(line.orders[0].ordinal, 0);
        assert_eq!(line.orders[0].offer_id, expected_first);
        assert_eq!(line.orders[1].ordinal, 1);
        assert!(engine.events.iter().any(|event| {
            event.event_type == "random-draw"
                && event_contains(event, "supplier-order-acceptance")
                && event_contains(event, "candidates")
        }));
    }

    #[test]
    fn fulfillment_shortfall_uses_ordinal_without_agent_id_fallback() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let component = engine.supplier_lines[0].component_id.clone();
        let supplier = engine.supplier_lines[0].supplier_id.clone();
        engine.supplier_lines[0].actual_capacity = 0;
        engine.supplier_lines[0].inventory = 4;
        engine.supplier_lines[0].next_ordinal = 2;
        engine.supplier_lines[0].orders = vec![
            SupplierOrder {
                id: "order-for-agent-a".into(),
                offer_id: "offer-for-agent-a".into(),
                agent_id: engine.agents[0].id.clone(),
                supplier_id: supplier.clone(),
                component_id: component.clone(),
                quantity: 4,
                due_day: 0,
                unit_price: 1,
                down_payment: 0,
                delivered: 0,
                ordinal: 1,
            },
            SupplierOrder {
                id: "order-for-agent-b".into(),
                offer_id: "offer-for-agent-b".into(),
                agent_id: engine.agents[1].id.clone(),
                supplier_id: supplier,
                component_id: component,
                quantity: 4,
                due_day: 0,
                unit_price: 1,
                down_payment: 0,
                delivered: 0,
                ordinal: 0,
            },
        ];
        engine.supplier_fulfillment().unwrap();
        let line = &engine.supplier_lines[0];
        let first = line.orders.iter().find(|order| order.ordinal == 0).unwrap();
        let second = line.orders.iter().find(|order| order.ordinal == 1).unwrap();
        assert_eq!(first.agent_id, "agent-b");
        assert_eq!(first.delivered, 4);
        assert_eq!(second.agent_id, "agent-a");
        assert_eq!(second.delivered, 0);
    }

    #[test]
    fn final_supplier_shortfall_uses_largest_remainder() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        engine.day = 219;
        let component = engine.supplier_lines[0].component_id.clone();
        engine.supplier_lines[0].inventory = 5;
        engine.supplier_lines[0].orders = vec![
            SupplierOrder {
                id: "order-a".into(),
                offer_id: "offer-a".into(),
                agent_id: engine.agents[0].id.clone(),
                supplier_id: engine.supplier_lines[0].supplier_id.clone(),
                component_id: component.clone(),
                quantity: 4,
                due_day: 219,
                unit_price: 100,
                down_payment: 100,
                delivered: 0,
                ordinal: 0,
            },
            SupplierOrder {
                id: "order-b".into(),
                offer_id: "offer-b".into(),
                agent_id: engine.agents[1].id.clone(),
                supplier_id: engine.supplier_lines[0].supplier_id.clone(),
                component_id: component,
                quantity: 4,
                due_day: 219,
                unit_price: 100,
                down_payment: 100,
                delivered: 0,
                ordinal: 1,
            },
        ];
        engine.final_settlement().unwrap();
        assert_eq!(engine.supplier_lines[0].orders[0].delivered, 3);
        assert_eq!(engine.supplier_lines[0].orders[1].delivered, 2);
        assert_eq!(
            engine
                .events
                .iter()
                .filter(|event| {
                    event.event_type == "economic-event" && event_contains(event, "supplier-refund")
                })
                .count(),
            2
        );
        assert!(engine.ledger_transactions.iter().any(|transaction| {
            transaction.reason == "supplier-refund"
                && transaction
                    .cause_id
                    .starts_with("supplier-refund/219/final-settlement/")
        }));

        let mut relabelled = GameEngine::new(plan()).unwrap();
        relabelled.day = 219;
        let component = relabelled.supplier_lines[0].component_id.clone();
        relabelled.supplier_lines[0].inventory = 5;
        relabelled.supplier_lines[0].next_ordinal = 2;
        relabelled.supplier_lines[0].orders = vec![
            SupplierOrder {
                id: "order-z".into(),
                offer_id: "offer-z".into(),
                agent_id: relabelled.agents[1].id.clone(),
                supplier_id: relabelled.supplier_lines[0].supplier_id.clone(),
                component_id: component.clone(),
                quantity: 4,
                due_day: 219,
                unit_price: 100,
                down_payment: 100,
                delivered: 0,
                ordinal: 0,
            },
            SupplierOrder {
                id: "order-a".into(),
                offer_id: "offer-a".into(),
                agent_id: relabelled.agents[0].id.clone(),
                supplier_id: relabelled.supplier_lines[0].supplier_id.clone(),
                component_id: component,
                quantity: 4,
                due_day: 219,
                unit_price: 100,
                down_payment: 100,
                delivered: 0,
                ordinal: 1,
            },
        ];
        relabelled.final_settlement().unwrap();
        assert_eq!(
            relabelled.supplier_lines[0]
                .orders
                .iter()
                .find(|order| order.ordinal == 0)
                .unwrap()
                .delivered,
            3
        );
        assert_eq!(
            relabelled.supplier_lines[0]
                .orders
                .iter()
                .find(|order| order.ordinal == 1)
                .unwrap()
                .delivered,
            2
        );
    }

    #[test]
    fn final_customer_settlement_has_exact_causes_entities_and_ledger_ids() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        engine.day = 219;
        let order = CustomerOrder {
            id: "customer-order/customer-rfq/210/low/0".into(),
            rfq_id: "customer-rfq/210/low/0".into(),
            agent_id: engine.agents[0].id.clone(),
            product_id: PRODUCTS[0].id.into(),
            quantity: 1,
            due_day: 215,
            unit_price: 100,
            penalty: 7,
            penalty_count: 3,
            status: CustomerOrderStatus::Open,
            delivery_day: None,
        };
        engine
            .entity_event(
                "customer-settlement",
                "customer-order",
                &order.id,
                "created",
                customer_order_value(&order, engine.day),
                EventVisibility::Private(order.agent_id.clone()),
            )
            .unwrap();
        engine.agents[0].customer_orders.push(order.clone());
        engine.final_settlement().unwrap();
        let settled = &engine.agents[0].customer_orders[0];
        assert_eq!(settled.penalty_count, 5);
        assert_eq!(settled.status, CustomerOrderStatus::Cancelled);
        assert_eq!(engine.customer_settlements[0].len(), 2);
        assert_eq!(engine.ledger_transactions.len(), 2);
        for transaction in &engine.ledger_transactions {
            assert!(
                transaction.cause_id == format!("customer-penalty/{}/4", order.id)
                    || transaction.cause_id == format!("customer-penalty/{}/5", order.id)
            );
            let expected = format!(
                "ledger-transaction/{}",
                hex(&sha256(
                    &Value::Array(vec![
                        Value::U64(219),
                        Value::Text("final-settlement".into()),
                        Value::Text(order.agent_id.clone()),
                        Value::Text("late-penalty".into()),
                        Value::Text(transaction.cause_id.clone()),
                    ])
                    .encode()
                ))
            );
            assert_eq!(transaction.id, expected);
        }
        assert_eq!(engine.ledger_accounts.values().sum::<i64>(), 0);
        assert!(engine.events.iter().any(|event| {
            event.event_type == "economic-event" && event_contains(event, "customer-cancellation")
        }));
        assert!(engine.events.iter().any(|event| {
            event.event_type == "ledger-transferred"
                && event_contains(event, &format!("customer-penalty/{}/5", order.id))
        }));
    }

    #[test]
    fn twenty_day_report_has_closed_aggregate_shape() {
        let mut engine = GameEngine::new(plan()).unwrap();
        for day in 0..20 {
            let observations = engine.open_day().unwrap().to_vec();
            if day == 19 {
                let report = observations[0].market_report.as_ref().unwrap();
                assert_eq!((report.start_day, report.end_day), (0, 19));
                assert_eq!(engine.market_start_day, 20);
                assert_eq!(report.products.len(), 16);
                assert_eq!(report.components.len(), 10);
                assert_eq!(report.supplier_lines.len(), engine.supplier_lines.len());
                assert!(
                    report
                        .supplier_lines
                        .iter()
                        .all(|line| line.mean_capacity_nano.is_some())
                );
                assert!(
                    report
                        .products
                        .iter()
                        .map(|item| item.rfq_count)
                        .sum::<u64>()
                        > 0
                );
                assert!(
                    report
                        .products
                        .iter()
                        .all(|item| item.mean_winning_unit_price.is_none())
                );
            }
            let inputs = observations
                .into_iter()
                .map(|observation| ActionInput::Default {
                    agent_id: observation.agent_id,
                    cause: DefaultCause::Missing,
                })
                .collect();
            engine.commit_day(inputs).unwrap();
        }
    }

    #[test]
    fn economic_rejection_is_private_and_observable() {
        let mut engine = GameEngine::new_with_event_capture(plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        let mut action = GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1);
        action.supplier_orders.push(SupplierOrderCommand {
            command_id: "bad-order".into(),
            offer_id: "supplier-offer/missing".into(),
        });
        engine.commit_day(defaults_except(&day0, action)).unwrap();
        let day1 = engine.open_day().unwrap();
        assert_eq!(day1[0].rejections[0].code, "expired");
        assert!(engine.events.iter().any(|event| {
            event.event_type == "economic-event"
                && event.visibility == EventVisibility::Private(engine.agents[0].id.clone())
        }));
    }

    #[test]
    fn observation_contains_stateless_recovery_surface() {
        let mut engine = GameEngine::new(plan()).unwrap();
        engine.agents[0].cash = -1_000_000_000;
        let day0 = engine.open_day().unwrap().to_vec();
        assert_eq!(day0[0].daily_prices.len(), 16);
        assert!(day0[0].event_cursor > 0);
        assert_eq!(day0[0].component_inventory, engine.agents[0].components);
        assert_eq!(day0[0].pc_inventory, engine.agents[0].pcs);
        assert_eq!(day0[0].ledger_since_previous.len(), 1);
        assert!(day0[0].previous_action.is_none());
        engine
            .commit_day(
                day0.iter()
                    .map(|observation| ActionInput::Default {
                        agent_id: observation.agent_id.clone(),
                        cause: DefaultCause::Timeout,
                    })
                    .collect(),
            )
            .unwrap();
        let day1 = engine.open_day().unwrap();
        assert_eq!(day1[0].previous_action.as_ref().unwrap().day, 0);
        assert_eq!(day1[0].previous_action.as_ref().unwrap().cause, "timeout");
        assert_eq!(day1[0].default_history, vec![(0, DefaultCause::Timeout)]);
        assert_eq!(
            day1[0].supplier_reputations.len(),
            engine.supplier_lines.len()
        );
    }

    #[test]
    fn every_economic_command_family_reports_rejection() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let observations = engine.open_day().unwrap().to_vec();
        let mut action = GameActionSet::empty(&observations[0].agent_id, &observations[0].id, 0, 1);
        action.customer_offers.push(CustomerOffer {
            command_id: "bad-customer".into(),
            rfq_id: "customer-rfq/missing".into(),
            unit_price: 1,
        });
        action.supplier_rfqs.push(SupplierRfqCommand {
            command_id: "bad-supplier".into(),
            rfq_id: "rfq".into(),
            supplier_id: "pintel".into(),
            component_id: "memory-1".into(),
            quantity: 1,
            due_day: 2,
            max_unit_price: None,
        });
        action.production_schedule.push(ProductionLine {
            line_id: "bad-production".into(),
            priority: 0,
            product_id: "missing".into(),
            quantity: 1,
        });
        action.delivery_schedule.push(DeliveryLine {
            line_id: "bad-delivery".into(),
            priority: 0,
            customer_order_id: "customer-order/missing".into(),
            quantity: 1,
        });
        engine
            .commit_day(defaults_except(&observations, action))
            .unwrap();
        let next = engine.open_day().unwrap();
        let codes: BTreeSet<_> = next[0]
            .rejections
            .iter()
            .map(|rejection| rejection.code)
            .collect();
        assert!(codes.contains("unknown-reference"));
        assert_eq!(next[0].rejections.len(), 4);
    }

    #[test]
    fn supplier_report_unit_price_is_quantity_weighted() {
        let mut engine = GameEngine::new(plan()).unwrap();
        let accumulator = engine.component_market.get_mut("memory-1").unwrap();
        accumulator.ordered_quantity = 3;
        accumulator.ordered_price_sum = 500;
        engine.day = 19;
        engine.publish_market_report().unwrap();
        let row = engine
            .current_market_report
            .as_ref()
            .unwrap()
            .components
            .iter()
            .find(|row| row.component_id == "memory-1")
            .unwrap();
        assert_eq!(row.mean_ordered_unit_price, Some(167));
    }

    // ---- Draft-3 rule sync ----

    fn draft3_plan() -> GamePlan {
        GamePlan::materialize_draft3([7_u8; 32]).unwrap()
    }

    fn supplier_rfq(
        command: &str,
        supplier: &str,
        component: &str,
        quantity: u32,
        due: u32,
    ) -> SupplierRfqCommand {
        SupplierRfqCommand {
            command_id: command.into(),
            rfq_id: command.into(),
            supplier_id: supplier.into(),
            component_id: component.into(),
            quantity,
            due_day: due,
            max_unit_price: None,
        }
    }

    fn entity_removals(engine: &GameEngine, entity_type: &str) -> Vec<String> {
        engine
            .events
            .iter()
            .filter(|event| event.event_type == "entity-changed")
            .filter_map(|event| {
                let (decoded, _) = crate::cbor::decode_one(&event.canonical_bytes).unwrap();
                let payload = map_field(&decoded, "payload");
                (map_field(payload, "entity_type") == &Value::Text(entity_type.into())
                    && map_field(payload, "operation") == &Value::Text("removed".into()))
                    .then(|| match map_field(payload, "entity_id") {
                        Value::Text(id) => id.clone(),
                        _ => panic!("entity id is text"),
                    })
            })
            .collect()
    }

    /// A deterministic test policy that, during the first 40 days, bids on two
    /// visible customer RFQs at their reserve, requests one component pair
    /// every day, and orders every positive supplier offer; it then idles to
    /// final settlement. A full game thus exercises the draft-3 paths without
    /// the quadratic state-commitment cost of a fully active 220-day ledger.
    fn exercising_inputs(observations: &[Observation]) -> Vec<ActionInput> {
        observations
            .iter()
            .enumerate()
            .map(|(slot, observation)| {
                let day = observation.day;
                let mut action =
                    GameActionSet::empty(&observation.agent_id, &observation.id, day, 1);
                if day >= 40 {
                    return ActionInput::Accepted(action);
                }
                action.customer_offers = observation
                    .customer_rfqs
                    .iter()
                    .skip(slot)
                    .step_by(6)
                    .take(2)
                    .enumerate()
                    .map(|(index, rfq)| CustomerOffer {
                        command_id: format!("c{index}"),
                        rfq_id: rfq.id.clone(),
                        unit_price: rfq.reserve,
                    })
                    .collect();
                action.supplier_orders = observation
                    .supplier_offers
                    .iter()
                    .filter(|offer| offer.kind == SupplierOfferKind::OnDate)
                    .enumerate()
                    .map(|(index, offer)| SupplierOrderCommand {
                        command_id: format!("o{index}"),
                        offer_id: offer.id.clone(),
                    })
                    .collect();
                if day + 5 <= 219 {
                    let component = &COMPONENTS[(slot + usize::try_from(day).unwrap()) % 10];
                    action.supplier_rfqs.push(supplier_rfq(
                        &format!("r{day}"),
                        component.suppliers[0],
                        component.id,
                        20,
                        day + 5,
                    ));
                }
                ActionInput::Accepted(action)
            })
            .collect()
    }

    fn run_exercising_game(plan: GamePlan) -> (GameEngine, GameResult, Vec<Vec<ActionInput>>) {
        let mut engine = GameEngine::new_with_event_capture(plan).unwrap();
        let mut inputs = Vec::new();
        loop {
            let observations = engine.open_day().unwrap().to_vec();
            let day_inputs = exercising_inputs(&observations);
            inputs.push(day_inputs.clone());
            if let Some(result) = engine.commit_day(day_inputs).unwrap() {
                return (engine, result, inputs);
            }
        }
    }

    #[test]
    fn draft3_plan_shares_draft2_streams_and_differs_only_in_identity() {
        for seed in [[0_u8; 32], [7_u8; 32]] {
            let draft2 = GamePlan::materialize(seed).unwrap();
            let draft3 = GamePlan::materialize_draft3(seed).unwrap();
            assert_eq!(draft3.profile_id, DRAFT3_PROFILE_ID);
            assert_eq!(draft3.profile_revision, DRAFT3_PROFILE_REVISION);
            assert_eq!(draft3.rules(), ProfileRules::DRAFT3);
            assert_eq!(draft2.rules(), ProfileRules::DRAFT2);
            assert_eq!(draft3.loan_rate_ppb, draft2.loan_rate_ppb);
            assert_eq!(draft3.deposit_rate_ppb, draft2.deposit_rate_ppb);
            assert_eq!(draft3.storage_rate_ppb, draft2.storage_rate_ppb);
            assert_eq!(draft3.initial_means_nano, draft2.initial_means_nano);
            assert_eq!(draft3.stream_patterns, draft2.stream_patterns);
            assert_eq!(draft3.engine_identity, sha256(b"tac-scm-engine-core/0.5.1"));
            assert_eq!(draft2.engine_identity, sha256(b"tac-scm-engine-core/0.4.1"));
            assert_ne!(draft3.configuration_identity, draft2.configuration_identity);
            assert_ne!(draft3.game_id, draft2.game_id);
            draft3.validate().unwrap();
            assert_eq!(
                GamePlan::materialize_profile(seed, DRAFT3_PROFILE_ID).unwrap(),
                draft3
            );
            assert_eq!(
                GamePlan::materialize_profile(seed, PROFILE_ID).unwrap(),
                draft2
            );
        }
        assert!(GamePlan::materialize_profile([0; 32], "tac-scm-2026-draft9").is_err());
        let mut relabelled = GamePlan::materialize([0; 32]).unwrap();
        relabelled.profile_id = DRAFT3_PROFILE_ID.into();
        relabelled.profile_revision = DRAFT3_PROFILE_REVISION;
        assert!(GameEngine::new(relabelled).is_err());
        let mut wrong_engine = GamePlan::materialize_draft3([0; 32]).unwrap();
        wrong_engine.engine_identity = sha256(ENGINE_SEMANTIC_ID.as_bytes());
        assert!(GameEngine::new(wrong_engine).is_err());
    }

    #[test]
    fn draft3_issues_no_day_zero_rfqs_and_keeps_later_demand() {
        let mut draft2 = GameEngine::new(plan()).unwrap();
        let mut draft3 = GameEngine::new(draft3_plan()).unwrap();
        for day in 0..6 {
            let left = draft2.open_day().unwrap().to_vec();
            let right = draft3.open_day().unwrap().to_vec();
            if day == 0 {
                assert!(!left[0].customer_rfqs.is_empty());
                assert!(
                    right
                        .iter()
                        .all(|observation| observation.customer_rfqs.is_empty())
                );
            } else {
                assert!(!right[0].customer_rfqs.is_empty());
                assert_eq!(left[0].customer_rfqs, right[0].customer_rfqs);
            }
            let walk = |engine: &GameEngine| {
                engine
                    .segments
                    .iter()
                    .map(|segment| (segment.q_nano, segment.trend_ppb))
                    .collect::<Vec<_>>()
            };
            assert_eq!(walk(&draft2), walk(&draft3));
            draft2
                .commit_day(defaults_except(
                    &left,
                    GameActionSet::empty(&left[0].agent_id, &left[0].id, day, 1),
                ))
                .unwrap();
            draft3
                .commit_day(defaults_except(
                    &right,
                    GameActionSet::empty(&right[0].agent_id, &right[0].id, day, 1),
                ))
                .unwrap();
        }
    }

    #[test]
    fn draft3_customer_rfq_takes_bids_only_on_its_issue_day() {
        let mut engine = GameEngine::new_with_event_capture(draft3_plan()).unwrap();
        let day0 = engine.open_day().unwrap().to_vec();
        engine
            .commit_day(defaults_except(
                &day0,
                GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1),
            ))
            .unwrap();
        let day1 = engine.open_day().unwrap().to_vec();
        let late = day1[0].customer_rfqs[0].clone();
        let same_day = day1[0].customer_rfqs[1].clone();
        // Day 1: bid on one RFQ the day it is issued; leave the other open.
        let mut bid = GameActionSet::empty(&day1[0].agent_id, &day1[0].id, 1, 1);
        bid.customer_offers.push(CustomerOffer {
            command_id: "same-day-bid".into(),
            rfq_id: same_day.id.clone(),
            unit_price: 1,
        });
        engine.commit_day(defaults_except(&day1, bid)).unwrap();
        assert!(engine.latest_rejections()[0].is_empty());
        let day2 = engine.open_day().unwrap().to_vec();
        assert_eq!(day2[0].new_customer_orders.len(), 1);
        assert_eq!(day2[0].new_customer_orders[0].rfq_id, same_day.id);
        // The unawarded day-1 RFQ closed at day-2 settlement, long before due.
        assert!(late.due_day > 2);
        assert!(
            engine
                .customer_rfqs
                .iter()
                .all(|row| !row.id.starts_with("customer-rfq/1/"))
        );
        assert_customer_rfq_removed_once(&engine, &late, 2);
        assert_customer_rfq_removed_once(&engine, &same_day, 2);
        let mut late_bid = GameActionSet::empty(&day2[0].agent_id, &day2[0].id, 2, 1);
        late_bid.customer_offers.push(CustomerOffer {
            command_id: "late-first-bid".into(),
            rfq_id: late.id.clone(),
            unit_price: 1,
        });
        engine.commit_day(defaults_except(&day2, late_bid)).unwrap();
        assert_eq!(engine.latest_rejections()[0].len(), 1);
        assert_eq!(engine.latest_rejections()[0][0].code, "unknown-reference");
        let day3 = engine.open_day().unwrap();
        assert!(day3[0].new_customer_orders.is_empty());
        validate_captured_events(&engine);
    }

    #[test]
    fn draft3_limits_supplier_rfqs_per_pair_and_rejects_the_whole_class() {
        let action_for = |observations: &[Observation]| {
            let mut action =
                GameActionSet::empty(&observations[0].agent_id, &observations[0].id, 0, 1);
            // Two components from one supplier: five each is within the
            // per-pair limit although ten exceed a per-supplier count.
            for index in 0..5 {
                action.supplier_rfqs.push(supplier_rfq(
                    &format!("pin{index}"),
                    "basus",
                    "board-pintel",
                    10,
                    5,
                ));
                action.supplier_rfqs.push(supplier_rfq(
                    &format!("imd{index}"),
                    "basus",
                    "board-imd",
                    10,
                    5,
                ));
            }
            action
                .supplier_rfqs
                .push(supplier_rfq("probe-a", "basus", "board-imd", 0, 6));
            // Six positive requests for one pair and two probes for another.
            for index in 0..6 {
                action.supplier_rfqs.push(supplier_rfq(
                    &format!("mem{index}"),
                    "mec",
                    "memory-1",
                    10,
                    5,
                ));
            }
            action
                .supplier_rfqs
                .push(supplier_rfq("probe-b", "mec", "memory-2", 0, 6));
            action
                .supplier_rfqs
                .push(supplier_rfq("probe-c", "mec", "memory-2", 0, 7));
            // An otherwise invalid request is rejected for its own reason and
            // does not count toward the limit.
            action
                .supplier_rfqs
                .push(supplier_rfq("late", "mec", "memory-2", 0, 1));
            action
        };
        let mut draft3 = GameEngine::new_with_event_capture(draft3_plan()).unwrap();
        let observations = draft3.open_day().unwrap().to_vec();
        draft3
            .commit_day(defaults_except(&observations, action_for(&observations)))
            .unwrap();
        let rejected = draft3.latest_rejections()[0]
            .iter()
            .map(|rejection| (rejection.command_id.as_str(), rejection.code))
            .collect::<BTreeMap<_, _>>();
        let mut expected = (0..6)
            .map(|index| (format!("mem{index}"), "cardinality"))
            .collect::<Vec<_>>();
        expected.push(("probe-b".into(), "cardinality"));
        expected.push(("probe-c".into(), "cardinality"));
        expected.push(("late".into(), "out-of-range"));
        assert_eq!(
            rejected,
            expected
                .iter()
                .map(|(id, code)| (id.as_str(), *code))
                .collect::<BTreeMap<_, _>>()
        );
        let next = draft3.open_day().unwrap().to_vec();
        let answered = next[0]
            .supplier_offers
            .iter()
            .map(|offer| offer.rfq_id.clone())
            .collect::<BTreeSet<_>>();
        for index in 0..5 {
            assert!(answered.contains(&supplier_rfq_id(
                "agent-a",
                "basus",
                &format!("pin{index}")
            )));
            assert!(answered.contains(&supplier_rfq_id(
                "agent-a",
                "basus",
                &format!("imd{index}")
            )));
        }
        assert!(answered.contains(&supplier_rfq_id("agent-a", "basus", "probe-a")));
        for id in ["mem0", "mem5", "probe-b", "probe-c"] {
            assert!(!answered.contains(&supplier_rfq_id("agent-a", "mec", id)));
        }
        validate_captured_events(&draft3);
        // Draft 2 keeps its unchanged behaviour: no limit is enforced.
        let mut draft2 = GameEngine::new(plan()).unwrap();
        let observations = draft2.open_day().unwrap().to_vec();
        draft2
            .commit_day(defaults_except(&observations, action_for(&observations)))
            .unwrap();
        assert_eq!(
            draft2.latest_rejections()[0]
                .iter()
                .map(|rejection| rejection.command_id.as_str())
                .collect::<Vec<_>>(),
            ["late"]
        );
    }

    #[test]
    fn draft3_delivered_supplier_order_is_observed_once_then_removed() {
        let mut engine = GameEngine::new_with_event_capture(draft3_plan()).unwrap();
        let mut observations = engine.open_day().unwrap().to_vec();
        let mut action = GameActionSet::empty(&observations[0].agent_id, &observations[0].id, 0, 1);
        action
            .supplier_rfqs
            .push(supplier_rfq("small", "mec", "memory-1", 5, 2));
        engine
            .commit_day(defaults_except(&observations, action))
            .unwrap();
        observations = engine.open_day().unwrap().to_vec();
        let offer = observations[0]
            .supplier_offers
            .iter()
            .find(|offer| offer.kind == SupplierOfferKind::OnDate)
            .expect("small request receives an on-date offer")
            .clone();
        assert_eq!(offer.quantity, 5);
        let mut order = GameActionSet::empty(&observations[0].agent_id, &observations[0].id, 1, 1);
        order.supplier_orders.push(SupplierOrderCommand {
            command_id: "buy".into(),
            offer_id: offer.id.clone(),
        });
        engine
            .commit_day(defaults_except(&observations, order))
            .unwrap();
        let mut delivered_day = None;
        for day in 2..10 {
            observations = engine.open_day().unwrap().to_vec();
            let orders = &observations[0].supplier_orders;
            if let Some(delivered) = delivered_day {
                assert!(
                    orders.is_empty(),
                    "day {day} still reports the delivered order"
                );
                assert!(day > delivered);
            } else if !observations[0].supplier_deliveries.is_empty() {
                assert_eq!(orders.len(), 1);
                assert_eq!(orders[0].delivered, orders[0].quantity);
                delivered_day = Some(day);
            } else {
                assert_eq!(orders.len(), 1);
            }
            engine
                .commit_day(defaults_except(
                    &observations,
                    GameActionSet::empty(&observations[0].agent_id, &observations[0].id, day, 1),
                ))
                .unwrap();
        }
        assert!(delivered_day.is_some());
        assert_eq!(entity_removals(&engine, "supplier-order").len(), 1);
        assert_eq!(engine.retired_supplier_units[0], 5);
        assert!(
            engine
                .supplier_lines
                .iter()
                .all(|line| line.orders.is_empty())
        );
        validate_captured_events(&engine);
    }

    #[test]
    fn draft3_rejects_a_second_delivery_line_for_one_order() {
        // Same action under both profiles: two delivery lines for one open
        // order, the earlier listed one with the later line id.
        let outcome = |plan: GamePlan| {
            let mut engine = GameEngine::new(plan).unwrap();
            let day0 = engine.open_day().unwrap().to_vec();
            let product = PRODUCTS[0].id;
            let agent_id = engine.agents[0].id.clone();
            engine.agents[0].customer_orders.push(CustomerOrder {
                id: "customer-order/customer-rfq/0/low/998".into(),
                rfq_id: "customer-rfq/0/low/998".into(),
                agent_id,
                product_id: product.into(),
                quantity: 2,
                due_day: 5,
                unit_price: 100,
                penalty: 17,
                penalty_count: 0,
                status: CustomerOrderStatus::Open,
                delivery_day: None,
            });
            engine.agents[0].pcs.insert(product.into(), 4);
            let mut action = GameActionSet::empty(&day0[0].agent_id, &day0[0].id, 0, 1);
            for line_id in ["z-first", "a-second"] {
                action.delivery_schedule.push(DeliveryLine {
                    line_id: line_id.into(),
                    priority: 0,
                    customer_order_id: "customer-order/customer-rfq/0/low/998".into(),
                    quantity: 2,
                });
            }
            engine.commit_day(defaults_except(&day0, action)).unwrap();
            let rejections = engine.latest_rejections()[0].clone();
            let day1 = engine.open_day().unwrap().to_vec();
            let executed: Vec<_> = day1[0]
                .delivery_results
                .iter()
                .filter(|result| result.executed)
                .map(|result| result.line_id.clone())
                .collect();
            (rejections, executed, engine.agents[0].pcs[product])
        };

        let (rejections, executed, pcs) = outcome(draft3_plan());
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].command_id, "a-second");
        assert_eq!(rejections[0].code, "cardinality");
        assert_eq!(rejections[0].field_path, "delivery_schedule");
        assert_eq!(executed, vec!["z-first".to_owned()]);
        assert_eq!(pcs, 2);

        // Draft 2 keeps its 0.4.1 semantics: both lines execute.
        let (rejections, executed, pcs) = outcome(GamePlan::materialize([7_u8; 32]).unwrap());
        assert!(rejections.is_empty());
        assert_eq!(executed.len(), 2);
        assert_eq!(pcs, 0);
    }

    #[test]
    fn draft3_complete_game_is_conformant_replayable_and_bounded() {
        let (engine, result, inputs) = run_exercising_game(draft3_plan());
        validate_captured_events(&engine);
        assert!(!entity_removals(&engine, "supplier-order").is_empty());
        let mut replay = GameEngine::new_with_event_capture(draft3_plan()).unwrap();
        let mut replayed = None;
        for day_inputs in inputs {
            replay.open_day().unwrap();
            replayed = replay.commit_day(day_inputs).unwrap();
        }
        compare_results(&replayed.unwrap(), &result).unwrap();
        // Only open orders remain; delivered ones were reported and retired.
        assert!(
            engine
                .supplier_lines
                .iter()
                .flat_map(|line| &line.orders)
                .all(|order| order.delivered < order.quantity)
        );
        // The same policy under draft 2 accumulates every order ever placed.
        let (draft2, _, _) = run_exercising_game(plan());
        let draft2_orders: usize = draft2
            .supplier_lines
            .iter()
            .map(|line| line.orders.len())
            .sum();
        let draft3_orders: usize = engine
            .supplier_lines
            .iter()
            .map(|line| line.orders.len())
            .sum();
        assert!(draft2_orders > 100, "{draft2_orders}");
        assert!(
            draft3_orders * 10 < draft2_orders,
            "{draft3_orders} vs {draft2_orders}"
        );
        // Receipt totals in the economic summary include retired orders.
        let received = |engine: &GameEngine, slot: usize| -> u64 {
            engine
                .supplier_lines
                .iter()
                .flat_map(|line| &line.orders)
                .filter(|order| order.agent_id == AGENT_IDS[slot])
                .map(|order| u64::from(order.delivered))
                .sum::<u64>()
                + engine.retired_supplier_units[slot]
        };
        assert!(received(&engine, 0) > 0);
    }

    #[test]
    fn draft2_default_game_is_unchanged_by_the_draft3_switches() {
        // The existing 0.4.1 locks cover the zero root; this adds the
        // exercising policy so every gated path is taken under draft 2 too.
        let (engine, result, _) = run_exercising_game(plan());
        validate_captured_events(&engine);
        assert!(entity_removals(&engine, "supplier-order").is_empty());
        assert_eq!(engine.retired_supplier_units, [0; 6]);
        assert!(result.event_count > 0);
    }
}
