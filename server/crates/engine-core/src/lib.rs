//! Authoritative TAC SCM 2026 draft-2 core.
//!
//! The crate contains the complete deterministic 220-day economic state machine
//! and a small architecture-gate transition fixture. Both remain transport-free:
//! storage, UI, wall-clock, agent runtime, and framework concerns live outside
//! the normative engine boundary.

pub mod arithmetic;
pub mod cbor;
pub mod conformance;
pub mod crypto;
pub mod game;
pub mod historical;
pub mod random;

use std::collections::BTreeSet;
use std::fmt;

use arithmetic::{ArithmeticError, daily_loan_interest};
use cbor::Value;
use crypto::{hex, sha256};
use random::{NamedStreams, RandomError};

/// The default normative profile. It also names the random-stream namespace
/// of every profile this engine runs, so one root seed materializes the same
/// exogenous market under each revision.
pub const PROFILE_ID: &str = "tac-scm-2026-draft2";
pub const PROFILE_REVISION: u32 = 2;
/// The explicitly selected rule-sync successor profile.
pub const DRAFT3_PROFILE_ID: &str = "tac-scm-2026-draft3";
pub const DRAFT3_PROFILE_REVISION: u32 = 1;
pub const SUPPLIER_MODULE_REVISION: u32 = 2;
pub const CONTRACTS_REVISION: u32 = 2;
pub const CONFORMANCE_REVISION: u32 = 2;
pub const ENGINE_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Semantic identity of the draft-2 and research profiles. Their state
/// machine is unchanged since 0.4.1, so their configuration identities, game
/// ids, and replay bytes stay those of 0.4.1 under later package versions.
pub const ENGINE_SEMANTIC_ID: &str = "tac-scm-engine-core/0.4.1";
/// Semantic identity of the draft-3 profile, derived from the package version.
pub const DRAFT3_ENGINE_SEMANTIC_ID: &str =
    concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
pub const EVENT_SCHEMA_ID: &str = "tac-scm-event/3";
pub const SLICE_SCHEMA_ID: &str = "tac-scm-architecture-slice/1";
pub const AGENT_IDS: [&str; 6] = [
    "agent-a", "agent-b", "agent-c", "agent-d", "agent-e", "agent-f",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceConfig {
    pub game_id: String,
    pub day: u32,
    pub observation_id: String,
    pub customer_rfq_id: String,
    pub customer_product_id: String,
    pub customer_quantity: u32,
    pub customer_due_day: u32,
    pub customer_daily_penalty: i64,
    pub annual_loan_rate_ppb: u32,
    pub root_seed: [u8; 32],
}

impl SliceConfig {
    #[must_use]
    pub fn fixture() -> Self {
        let mut root_seed = [0_u8; 32];
        for (index, byte) in root_seed.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("seed index fits u8");
        }
        Self {
            game_id: format!("plan/{}", "0".repeat(64)),
            day: 0,
            observation_id: "observation/0/fixture".into(),
            customer_rfq_id: "customer-rfq/0/low/0".into(),
            customer_product_id: "p01".into(),
            customer_quantity: 2,
            customer_due_day: 4,
            customer_daily_penalty: 400_000_000,
            annual_loan_rate_ppb: 100_000_000,
            root_seed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayActionSet {
    pub agent_id: String,
    pub observation_id: String,
    pub day: u32,
    pub revision: u32,
    pub command_id: String,
    pub unit_price_microcredits: i64,
}

impl DayActionSet {
    #[must_use]
    pub fn canonical_value(&self, rfq_id: &str) -> Value {
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
                Value::Array(vec![Value::Map(vec![
                    ("command_id".into(), Value::Text(self.command_id.clone())),
                    ("rfq_id".into(), Value::Text(rfq_id.into())),
                    (
                        "unit_price".into(),
                        Value::I64(self.unit_price_microcredits),
                    ),
                ])]),
            ),
            ("supplier_rfqs".into(), Value::Array(Vec::new())),
            ("supplier_orders".into(), Value::Array(Vec::new())),
            ("production_schedule".into(), Value::Array(Vec::new())),
            ("delivery_schedule".into(), Value::Array(Vec::new())),
        ])
    }

    #[must_use]
    pub fn digest(&self, rfq_id: &str) -> [u8; 32] {
        sha256(&self.canonical_value(rfq_id).encode())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedAction {
    pub action: DayActionSet,
    pub digest: [u8; 32],
}

impl CommittedAction {
    fn canonical_value(&self, rfq_id: &str) -> Value {
        Value::Map(vec![
            (
                "state".into(),
                Value::Map(vec![
                    ("day".into(), Value::U64(u64::from(self.action.day))),
                    ("agent_id".into(), Value::Text(self.action.agent_id.clone())),
                    ("action_digest".into(), Value::Bytes(self.digest.to_vec())),
                    ("cause".into(), Value::Text("agent-submission".into())),
                ]),
            ),
            ("action".into(), self.action.canonical_value(rfq_id)),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerOrder {
    pub order_id: String,
    pub rfq_id: String,
    pub agent_id: String,
    pub product_id: String,
    pub quantity: u32,
    pub due_day: u32,
    pub unit_price_microcredits: i64,
    pub daily_penalty_microcredits: i64,
}

impl CustomerOrder {
    fn canonical_value(&self) -> Value {
        Value::Map(vec![
            ("order_id".into(), Value::Text(self.order_id.clone())),
            ("rfq_id".into(), Value::Text(self.rfq_id.clone())),
            ("agent_id".into(), Value::Text(self.agent_id.clone())),
            ("product_id".into(), Value::Text(self.product_id.clone())),
            ("quantity".into(), Value::U64(u64::from(self.quantity))),
            ("due_day".into(), Value::U64(u64::from(self.due_day))),
            (
                "unit_price".into(),
                Value::I64(self.unit_price_microcredits),
            ),
            (
                "daily_penalty".into(),
                Value::I64(self.daily_penalty_microcredits),
            ),
            ("penalty_count".into(), Value::U64(0)),
            ("status".into(), Value::Text("open".into())),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceState {
    pub day: u32,
    pub cash_microcredits: [i64; 6],
    pub accepted_action_digests: [[u8; 32]; 6],
    pub customer_order: Option<CustomerOrder>,
    pub tie_stream_byte_position: u64,
}

impl SliceState {
    fn canonical_value(&self, game_id: &str) -> Value {
        let agents = AGENT_IDS
            .iter()
            .zip(self.cash_microcredits)
            .zip(self.accepted_action_digests)
            .map(|((agent_id, cash), digest)| {
                Value::Map(vec![
                    ("agent_id".into(), Value::Text((*agent_id).into())),
                    ("cash_microcredits".into(), Value::I64(cash)),
                    ("action_digest".into(), Value::Bytes(digest.to_vec())),
                ])
            })
            .collect();
        Value::Map(vec![
            ("schema_id".into(), Value::Text(SLICE_SCHEMA_ID.into())),
            ("game_id".into(), Value::Text(game_id.into())),
            ("day".into(), Value::U64(u64::from(self.day))),
            ("agents".into(), Value::Array(agents)),
            (
                "customer_order".into(),
                self.customer_order
                    .as_ref()
                    .map_or(Value::Null, CustomerOrder::canonical_value),
            ),
            (
                "tie_stream_byte_position".into(),
                Value::U64(self.tie_stream_byte_position),
            ),
        ])
    }

    #[must_use]
    pub fn commitment(&self, game_id: &str) -> [u8; 32] {
        sha256(&self.canonical_value(game_id).encode())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRecord {
    pub index: u64,
    pub event_type: &'static str,
    pub previous_hash: [u8; 32],
    pub state_commitment: Option<[u8; 32]>,
    pub hash: [u8; 32],
    pub canonical_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceOutput {
    pub state: SliceState,
    pub transcript: Vec<CommittedAction>,
    pub transcript_bytes: Vec<u8>,
    pub transcript_sha256: [u8; 32],
    pub events: Vec<EventRecord>,
    pub event_log_bytes: Vec<u8>,
    pub event_log_sha256: [u8; 32],
    pub final_state_sha256: [u8; 32],
}

impl SliceOutput {
    /// Read-only visualizer boundary: canonical public events in log order.
    pub fn visualizer_events(&self) -> impl Iterator<Item = &[u8]> {
        self.events
            .iter()
            .map(|event| event.canonical_bytes.as_slice())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SliceError {
    WrongActionCount { actual: usize },
    UnknownAgent(String),
    DuplicateAgent(String),
    WrongDay { agent: String, actual: u32 },
    WrongObservation { agent: String },
    BadRevision { agent: String },
    InvalidIdentifier { field: &'static str, value: String },
    NegativePrice { agent: String },
    Arithmetic(ArithmeticError),
    Random(RandomError),
    ReplayMismatch { field: &'static str },
}

impl fmt::Display for SliceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongActionCount { actual } => {
                write!(formatter, "expected six actions, got {actual}")
            }
            Self::UnknownAgent(agent) => write!(formatter, "unknown agent {agent}"),
            Self::DuplicateAgent(agent) => write!(formatter, "duplicate agent {agent}"),
            Self::WrongDay { agent, actual } => write!(formatter, "wrong day {actual} for {agent}"),
            Self::WrongObservation { agent } => write!(formatter, "wrong observation for {agent}"),
            Self::BadRevision { agent } => {
                write!(formatter, "revision must start at one for {agent}")
            }
            Self::InvalidIdentifier { field, value } => {
                write!(formatter, "invalid {field} identifier {value}")
            }
            Self::NegativePrice { agent } => {
                write!(formatter, "negative customer offer from {agent}")
            }
            Self::Arithmetic(error) => write!(formatter, "{error}"),
            Self::Random(error) => write!(formatter, "{error:?}"),
            Self::ReplayMismatch { field } => write!(formatter, "replay mismatch at {field}"),
        }
    }
}

impl std::error::Error for SliceError {}

impl From<ArithmeticError> for SliceError {
    fn from(error: ArithmeticError) -> Self {
        Self::Arithmetic(error)
    }
}

impl From<RandomError> for SliceError {
    fn from(error: RandomError) -> Self {
        Self::Random(error)
    }
}

#[derive(Clone, Debug)]
struct EventWriter<'a> {
    config: &'a SliceConfig,
    events: Vec<EventRecord>,
    previous_hash: [u8; 32],
}

impl<'a> EventWriter<'a> {
    fn new(config: &'a SliceConfig) -> Self {
        Self {
            config,
            events: Vec::new(),
            previous_hash: [0_u8; 32],
        }
    }

    fn push(
        &mut self,
        event_type: &'static str,
        phase: &'static str,
        causes: Vec<Value>,
        payload: Value,
        state_commitment: Option<[u8; 32]>,
    ) {
        let index = u64::try_from(self.events.len()).expect("event count fits u64");
        let mut fields = vec![
            ("schema_id".into(), Value::Text(EVENT_SCHEMA_ID.into())),
            ("event_index".into(), Value::U64(index)),
            ("event_type".into(), Value::Text(event_type.into())),
            ("game_id".into(), Value::Text(self.config.game_id.clone())),
            ("day".into(), Value::U64(u64::from(self.config.day))),
            ("phase".into(), Value::Text(phase.into())),
            ("causes".into(), Value::Array(causes)),
            (
                "visibility".into(),
                Value::Map(vec![
                    ("kind".into(), Value::Text("public".into())),
                    ("audience".into(), Value::Array(Vec::new())),
                ]),
            ),
            ("payload".into(), payload),
            (
                "previous_event_hash".into(),
                Value::Bytes(self.previous_hash.to_vec()),
            ),
        ];
        if let Some(commitment) = state_commitment {
            fields.push(("state_commitment".into(), Value::Bytes(commitment.to_vec())));
        }
        let hash = sha256(&Value::Map(fields.clone()).encode());
        fields.push(("event_hash".into(), Value::Bytes(hash.to_vec())));
        let canonical_bytes = Value::Map(fields).encode();
        self.events.push(EventRecord {
            index,
            event_type,
            previous_hash: self.previous_hash,
            state_commitment,
            hash,
            canonical_bytes,
        });
        self.previous_hash = hash;
    }

    fn phase_opened(&mut self, phase: &'static str) {
        self.push(
            "phase-opened",
            phase,
            Vec::new(),
            Value::Map(vec![("phase".into(), Value::Text(phase.into()))]),
            None,
        );
    }

    fn phase_closed(&mut self, phase: &'static str, state_commitment: [u8; 32]) {
        self.push(
            "phase-closed",
            phase,
            Vec::new(),
            Value::Map(vec![("phase".into(), Value::Text(phase.into()))]),
            Some(state_commitment),
        );
    }
}

/// Creates deterministic fixture actions with a two-agent lowest-price tie.
#[must_use]
pub fn fixture_actions(config: &SliceConfig) -> Vec<DayActionSet> {
    AGENT_IDS
        .iter()
        .enumerate()
        .map(|(index, agent_id)| DayActionSet {
            agent_id: (*agent_id).into(),
            observation_id: config.observation_id.clone(),
            day: config.day,
            revision: 1,
            command_id: format!("offer-{index}"),
            unit_price_microcredits: if index < 2 {
                1_850_000_000
            } else {
                1_900_000_000 + i64::try_from(index).expect("small fixture index")
            },
        })
        .collect()
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), SliceError> {
    let valid_len = (1..=32).contains(&value.len());
    let mut bytes = value.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let valid_rest = bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    });
    if valid_len && valid_first && valid_rest {
        Ok(())
    } else {
        Err(SliceError::InvalidIdentifier {
            field,
            value: value.into(),
        })
    }
}

fn validate_and_sort_actions(
    config: &SliceConfig,
    actions: &[DayActionSet],
) -> Result<Vec<DayActionSet>, SliceError> {
    if actions.len() != AGENT_IDS.len() {
        return Err(SliceError::WrongActionCount {
            actual: actions.len(),
        });
    }
    let expected: BTreeSet<&str> = AGENT_IDS.into_iter().collect();
    let mut seen = BTreeSet::new();
    for action in actions {
        validate_identifier("agent", &action.agent_id)?;
        validate_identifier("command", &action.command_id)?;
        if !expected.contains(action.agent_id.as_str()) {
            return Err(SliceError::UnknownAgent(action.agent_id.clone()));
        }
        if !seen.insert(action.agent_id.as_str()) {
            return Err(SliceError::DuplicateAgent(action.agent_id.clone()));
        }
        if action.day != config.day {
            return Err(SliceError::WrongDay {
                agent: action.agent_id.clone(),
                actual: action.day,
            });
        }
        if action.observation_id != config.observation_id {
            return Err(SliceError::WrongObservation {
                agent: action.agent_id.clone(),
            });
        }
        if action.revision == 0 {
            return Err(SliceError::BadRevision {
                agent: action.agent_id.clone(),
            });
        }
        if action.unit_price_microcredits < 0 {
            return Err(SliceError::NegativePrice {
                agent: action.agent_id.clone(),
            });
        }
    }
    let mut sorted = actions.to_vec();
    sorted.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    Ok(sorted)
}

fn event_payload_ledger(config: &SliceConfig, agent_id: &str, amount: i64) -> (String, Value) {
    let cause_id = format!("finance-open/{}/{agent_id}", config.day);
    let tuple = Value::Array(vec![
        Value::U64(u64::from(config.day)),
        Value::Text("finance-open".into()),
        Value::Text(agent_id.into()),
        Value::Text("bank-interest".into()),
        Value::Text(cause_id.clone()),
    ]);
    let transaction_id = format!("ledger-transaction/{}", hex(&sha256(&tuple.encode())));
    let transaction = Value::Map(vec![
        ("transaction_id".into(), Value::Text(transaction_id)),
        ("day".into(), Value::U64(u64::from(config.day))),
        ("phase".into(), Value::Text("finance-open".into())),
        (
            "source_account".into(),
            Value::Text(format!("agent-cash/{agent_id}")),
        ),
        (
            "destination_account".into(),
            Value::Text("bank-interest".into()),
        ),
        ("amount".into(), Value::I64(amount)),
        ("reason".into(), Value::Text("bank-interest".into())),
        ("cause_id".into(), Value::Text(cause_id.clone())),
    ]);
    (
        cause_id,
        Value::Map(vec![("transaction".into(), transaction)]),
    )
}

/// Runs the bounded transition after validating all six action sets atomically.
pub fn run_slice(
    config: &SliceConfig,
    opening_cash_microcredits: [i64; 6],
    actions: &[DayActionSet],
) -> Result<SliceOutput, SliceError> {
    let sorted = validate_and_sort_actions(config, actions)?;
    let transcript: Vec<CommittedAction> = sorted
        .into_iter()
        .map(|action| CommittedAction {
            digest: action.digest(&config.customer_rfq_id),
            action,
        })
        .collect();
    let mut event_writer = EventWriter::new(config);
    let mut state = SliceState {
        day: config.day,
        cash_microcredits: opening_cash_microcredits,
        accepted_action_digests: [[0_u8; 32]; 6],
        customer_order: None,
        tie_stream_byte_position: 0,
    };

    event_writer.phase_opened("finance-open");
    for (index, agent_id) in AGENT_IDS.iter().enumerate() {
        let amount =
            daily_loan_interest(state.cash_microcredits[index], config.annual_loan_rate_ppb)?;
        if amount > 0 {
            state.cash_microcredits[index] = state.cash_microcredits[index]
                .checked_sub(amount)
                .ok_or(ArithmeticError::Overflow)?;
            let (cause_id, payload) = event_payload_ledger(config, agent_id, amount);
            event_writer.push(
                "ledger-transferred",
                "finance-open",
                vec![Value::Text(cause_id)],
                payload,
                None,
            );
        }
    }
    event_writer.phase_closed("finance-open", state.commitment(&config.game_id));

    event_writer.phase_opened("action-commit");
    for (index, committed) in transcript.iter().enumerate() {
        state.accepted_action_digests[index] = committed.digest;
        let disposition = Value::Map(vec![
            ("day".into(), Value::U64(u64::from(config.day))),
            (
                "action_digest".into(),
                Value::Bytes(committed.digest.to_vec()),
            ),
            ("cause".into(), Value::Text("agent-submission".into())),
            (
                "revision".into(),
                Value::U64(u64::from(committed.action.revision)),
            ),
        ]);
        event_writer.push(
            "action-committed",
            "action-commit",
            vec![Value::Text(committed.action.command_id.clone())],
            Value::Map(vec![
                (
                    "agent_id".into(),
                    Value::Text(committed.action.agent_id.clone()),
                ),
                ("disposition".into(), disposition),
            ]),
            None,
        );
    }

    let lowest_price = transcript
        .iter()
        .map(|committed| committed.action.unit_price_microcredits)
        .min()
        .expect("six validated actions");
    let tied: Vec<(String, &CommittedAction)> = transcript
        .iter()
        .filter(|committed| committed.action.unit_price_microcredits == lowest_price)
        .map(|committed| {
            (
                format!(
                    "customer-bid/{}",
                    hex(&sha256(
                        &Value::Array(vec![
                            Value::Text(committed.action.agent_id.clone()),
                            Value::Text(config.customer_rfq_id.clone()),
                            Value::U64(u64::from(config.day)),
                        ])
                        .encode(),
                    ))
                ),
                committed,
            )
        })
        .collect();
    let mut streams = NamedStreams::new(config.root_seed, PROFILE_ID);
    let permutation = streams.uniform_permutation(
        &format!(
            "symmetry/{}/action-commit/customer-award/{}",
            config.day, config.customer_rfq_id
        ),
        tied.iter().map(|(id, _)| id.clone()).collect(),
    )?;
    for selection in &permutation.selections {
        state.tie_stream_byte_position +=
            8 * u64::try_from(selection.rejected_draws.len() + 1).expect("draw count fits u64");
        for draw in &selection.rejected_draws {
            event_writer.push(
                "random-draw",
                "action-commit",
                vec![Value::Text(config.customer_rfq_id.clone())],
                Value::Map(vec![
                    ("stream_name".into(), Value::Text(draw.stream.clone())),
                    ("ordinal".into(), Value::U64(draw.ordinal)),
                    ("raw_u64".into(), Value::U64(draw.raw)),
                    ("result".into(), Value::I64(-1)),
                ]),
                None,
            );
        }
        event_writer.push(
            "random-draw",
            "action-commit",
            vec![Value::Text(config.customer_rfq_id.clone())],
            Value::Map(vec![
                (
                    "stream_name".into(),
                    Value::Text(selection.accepted_draw.stream.clone()),
                ),
                (
                    "ordinal".into(),
                    Value::U64(selection.accepted_draw.ordinal),
                ),
                ("raw_u64".into(), Value::U64(selection.accepted_draw.raw)),
                (
                    "result".into(),
                    Value::Map(vec![
                        (
                            "candidates".into(),
                            Value::Array(
                                selection
                                    .candidates
                                    .iter()
                                    .cloned()
                                    .map(Value::Text)
                                    .collect(),
                            ),
                        ),
                        (
                            "selected_index".into(),
                            Value::U64(u64::try_from(selection.selected_index).unwrap()),
                        ),
                        ("winner".into(), Value::Text(selection.winner.clone())),
                    ]),
                ),
            ]),
            None,
        );
    }
    let winning_bid_id = &permutation.order[0];
    let winner = tied
        .iter()
        .find_map(|(id, committed)| (id == winning_bid_id).then_some(*committed))
        .expect("winning bid belongs to a tied action");

    let customer_order = CustomerOrder {
        order_id: format!("customer-order/{}", config.customer_rfq_id),
        rfq_id: config.customer_rfq_id.clone(),
        agent_id: winner.action.agent_id.clone(),
        product_id: config.customer_product_id.clone(),
        quantity: config.customer_quantity,
        due_day: config.customer_due_day,
        unit_price_microcredits: lowest_price,
        daily_penalty_microcredits: config.customer_daily_penalty,
    };
    let order_bytes = customer_order.canonical_value().encode();
    let order_digest = sha256(&order_bytes);
    event_writer.push(
        "entity-changed",
        "action-commit",
        vec![Value::Text(winner.action.command_id.clone())],
        Value::Map(vec![
            ("entity_type".into(), Value::Text("customer-order".into())),
            (
                "entity_id".into(),
                Value::Text(customer_order.order_id.clone()),
            ),
            ("operation".into(), Value::Text("created".into())),
            ("value".into(), Value::Bytes(order_bytes)),
            ("value_sha256".into(), Value::Bytes(order_digest.to_vec())),
        ]),
        None,
    );
    state.customer_order = Some(customer_order);
    let final_state_sha256 = state.commitment(&config.game_id);
    event_writer.phase_closed("action-commit", final_state_sha256);

    let transcript_value = Value::Array(
        transcript
            .iter()
            .map(|committed| committed.canonical_value(&config.customer_rfq_id))
            .collect(),
    );
    let transcript_bytes = transcript_value.encode();
    let transcript_sha256 = sha256(&transcript_bytes);
    let event_log_bytes: Vec<u8> = event_writer
        .events
        .iter()
        .flat_map(|event| event.canonical_bytes.iter().copied())
        .collect();
    let event_log_sha256 = sha256(&event_log_bytes);

    Ok(SliceOutput {
        state,
        transcript,
        transcript_bytes,
        transcript_sha256,
        events: event_writer.events,
        event_log_bytes,
        event_log_sha256,
        final_state_sha256,
    })
}

/// Recomputes the transition and reports the first artifact-level mismatch.
pub fn verify_replay(
    config: &SliceConfig,
    opening_cash_microcredits: [i64; 6],
    actions: &[DayActionSet],
    expected: &SliceOutput,
) -> Result<(), SliceError> {
    let actual = run_slice(config, opening_cash_microcredits, actions)?;
    for (field, matches) in [
        (
            "accepted-action transcript",
            actual.transcript_bytes == expected.transcript_bytes,
        ),
        (
            "canonical event log",
            actual.event_log_bytes == expected.event_log_bytes,
        ),
        (
            "final state",
            actual.final_state_sha256 == expected.final_state_sha256,
        ),
    ] {
        if !matches {
            return Err(SliceError::ReplayMismatch { field });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AGENT_IDS, DRAFT3_ENGINE_SEMANTIC_ID, DRAFT3_PROFILE_ID, DRAFT3_PROFILE_REVISION,
        ENGINE_PACKAGE_VERSION, ENGINE_SEMANTIC_ID, EVENT_SCHEMA_ID, PROFILE_ID, PROFILE_REVISION,
        SliceConfig, SliceError, fixture_actions, run_slice, verify_replay,
    };
    use crate::crypto::hex;

    const OPENING_CASH: [i64; 6] = [-1_000_000_000; 6];

    #[test]
    fn semantic_identity_is_derived_from_the_aligned_cargo_version() {
        assert_eq!(ENGINE_PACKAGE_VERSION, "0.5.1");
        // Draft 2 keeps its 0.4.1 semantic identity; draft 3 carries 0.5.1.
        assert_eq!(ENGINE_SEMANTIC_ID, "tac-scm-engine-core/0.4.1");
        assert_eq!(DRAFT3_ENGINE_SEMANTIC_ID, "tac-scm-engine-core/0.5.1");
        assert_eq!(PROFILE_ID, "tac-scm-2026-draft2");
        assert_eq!(PROFILE_REVISION, 2);
        assert_eq!(DRAFT3_PROFILE_ID, "tac-scm-2026-draft3");
        assert_eq!(DRAFT3_PROFILE_REVISION, 1);
        assert_eq!(EVENT_SCHEMA_ID, "tac-scm-event/3");
    }

    #[test]
    fn six_agent_exchange_is_repeatable_and_replayable() {
        let config = SliceConfig::fixture();
        let actions = fixture_actions(&config);
        let first = run_slice(&config, OPENING_CASH, &actions).unwrap();
        let second = run_slice(&config, OPENING_CASH, &actions).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.transcript.len(), 6);
        assert_eq!(first.state.cash_microcredits, [-1_000_454_545; 6]);
        assert!(matches!(
            first
                .state
                .customer_order
                .as_ref()
                .map(|order| order.agent_id.as_str()),
            Some("agent-a" | "agent-b")
        ));
        assert_eq!(first.visualizer_events().count(), first.events.len());
        verify_replay(&config, OPENING_CASH, &actions, &first).unwrap();
    }

    #[test]
    fn action_arrival_order_does_not_change_output() {
        let config = SliceConfig::fixture();
        let actions = fixture_actions(&config);
        let forward = run_slice(&config, OPENING_CASH, &actions).unwrap();
        let reversed: Vec<_> = actions.into_iter().rev().collect();
        let backward = run_slice(&config, OPENING_CASH, &reversed).unwrap();
        assert_eq!(forward, backward);
    }

    #[test]
    fn structural_failure_is_atomic_and_specific() {
        let config = SliceConfig::fixture();
        let mut actions = fixture_actions(&config);
        actions[2].day = 1;
        assert_eq!(
            run_slice(&config, OPENING_CASH, &actions),
            Err(SliceError::WrongDay {
                agent: AGENT_IDS[2].into(),
                actual: 1,
            })
        );
    }

    #[test]
    fn event_chain_and_commitments_are_complete() {
        let config = SliceConfig::fixture();
        let output = run_slice(&config, OPENING_CASH, &fixture_actions(&config)).unwrap();
        let mut previous = [0_u8; 32];
        for (index, event) in output.events.iter().enumerate() {
            assert_eq!(event.index, u64::try_from(index).unwrap());
            assert_eq!(event.previous_hash, previous);
            previous = event.hash;
        }
        assert_eq!(
            output.events.last().unwrap().state_commitment,
            Some(output.final_state_sha256)
        );
        assert_ne!(hex(&output.event_log_sha256), "0".repeat(64));
    }

    #[test]
    fn transcript_change_fails_replay() {
        let config = SliceConfig::fixture();
        let actions = fixture_actions(&config);
        let output = run_slice(&config, OPENING_CASH, &actions).unwrap();
        let mut changed = actions;
        changed[0].unit_price_microcredits -= 1;
        assert_eq!(
            verify_replay(&config, OPENING_CASH, &changed, &output),
            Err(SliceError::ReplayMismatch {
                field: "accepted-action transcript"
            })
        );
    }
}
