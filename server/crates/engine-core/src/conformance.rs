//! Fail-closed validation of a complete canonical engine event sequence.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use crate::EVENT_SCHEMA_ID;
use crate::cbor::{Value, decode_one};
use crate::crypto::{hex, sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceReport {
    pub event_count: u64,
    pub entity_id_count: u64,
    pub economic_id_count: u64,
    pub ledger_transaction_count: u64,
    pub entity_types: BTreeSet<String>,
    pub economic_kinds: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceError(String);

impl fmt::Display for ConformanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ConformanceError {}

fn fail<T>(message: impl Into<String>) -> Result<T, ConformanceError> {
    Err(ConformanceError(message.into()))
}

fn map<'a>(value: &'a Value, context: &str) -> Result<&'a [(String, Value)], ConformanceError> {
    match value {
        Value::Map(fields) => Ok(fields),
        _ => fail(format!("{context} is not a map")),
    }
}

fn field<'a>(
    fields: &'a [(String, Value)],
    name: &str,
    context: &str,
) -> Result<&'a Value, ConformanceError> {
    fields
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value))
        .ok_or_else(|| ConformanceError(format!("{context} is missing {name}")))
}

fn optional_field<'a>(fields: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    fields
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn text<'a>(value: &'a Value, context: &str) -> Result<&'a str, ConformanceError> {
    match value {
        Value::Text(value) => Ok(value),
        _ => fail(format!("{context} is not text")),
    }
}

fn bytes<'a>(value: &'a Value, context: &str) -> Result<&'a [u8], ConformanceError> {
    match value {
        Value::Bytes(value) => Ok(value),
        _ => fail(format!("{context} is not bytes")),
    }
}

fn array<'a>(value: &'a Value, context: &str) -> Result<&'a [Value], ConformanceError> {
    match value {
        Value::Array(values) => Ok(values),
        _ => fail(format!("{context} is not an array")),
    }
}

fn unsigned(value: &Value, context: &str) -> Result<u64, ConformanceError> {
    match value {
        Value::U64(value) => Ok(*value),
        Value::I64(value) if *value >= 0 => Ok(u64::try_from(*value).unwrap()),
        _ => fail(format!("{context} is not an unsigned integer")),
    }
}

fn closed(
    fields: &[(String, Value)],
    required: &[&str],
    optional: &[&str],
    context: &str,
) -> Result<(), ConformanceError> {
    let actual = fields
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let required = required.iter().copied().collect::<BTreeSet<_>>();
    let allowed = required
        .iter()
        .copied()
        .chain(optional.iter().copied())
        .collect::<BTreeSet<_>>();
    if !required.is_subset(&actual) || !actual.is_subset(&allowed) {
        return fail(format!(
            "{context} is not closed: actual={actual:?} required={required:?} allowed={allowed:?}"
        ));
    }
    Ok(())
}

fn valid_engine_id(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'.' | b'_' | b'/' | b'-' => index > 0,
            _ => false,
        })
}

fn embedded_value(fields: &[(String, Value)], context: &str) -> Result<Value, ConformanceError> {
    let encoded = bytes(field(fields, "value", context)?, context)?;
    let expected = bytes(field(fields, "value_sha256", context)?, context)?;
    if expected != sha256(encoded) {
        return fail(format!("{context} value hash mismatch"));
    }
    let (value, consumed) = decode_one(encoded)
        .map_err(|error| ConformanceError(format!("{context} value CBOR: {error}")))?;
    if consumed != encoded.len() || value.encode() != encoded {
        return fail(format!("{context} value is not one canonical CBOR item"));
    }
    Ok(value)
}

fn entity_value_contract(
    entity_type: &str,
) -> Option<(
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
)> {
    match entity_type {
        "agent-state" => Some((
            &[
                "state_id",
                "agent_id",
                "cash",
                "components",
                "pcs",
                "pending_components",
                "pending_pcs",
                "production_schedule",
                "delivery_schedule",
                "production_results",
                "delivery_results",
                "supplier_rfqs",
                "supplier_orders",
                "supplier_deliveries",
                "customer_offers",
                "customer_orders",
                "new_customer_order_ids",
                "customer_settlements",
                "pending_command_rejections",
                "last_observation_id",
                "default_history",
            ],
            &[],
            "state_id",
        )),
        "supplier-line-state" => Some((
            &[
                "line_id",
                "supplier_id",
                "component_id",
                "actual_capacity",
                "expected_capacity",
                "willing_capacity",
                "inventory",
                "acceptance_ordinal_next",
                "orders",
                "pending_rfqs",
                "pending_offers",
                "reputations",
            ],
            &[],
            "line_id",
        )),
        "supplier-rfq" => Some((
            &[
                "rfq_id",
                "agent_id",
                "client_rfq_id",
                "supplier_id",
                "component_id",
                "quantity",
                "due_day",
                "max_unit_price",
            ],
            &[],
            "rfq_id",
        )),
        "supplier-offer" => Some((
            &[
                "offer_id",
                "alternative_group_id",
                "rfq_id",
                "agent_id",
                "supplier_id",
                "component_id",
                "kind",
                "quantity",
                "due_day",
                "unit_price",
                "valid_day",
                "reputation_exposure",
                "no_offer_reason",
            ],
            &[],
            "offer_id",
        )),
        "supplier-order" => Some((
            &[
                "order_id",
                "offer_id",
                "agent_id",
                "supplier_id",
                "component_id",
                "quantity",
                "due_day",
                "unit_price",
                "down_payment",
                "acceptance_ordinal",
                "delivered_quantity",
            ],
            &[],
            "order_id",
        )),
        "supplier-delivery" => Some((
            &[
                "delivery_id",
                "order_id",
                "component_id",
                "quantity",
                "unit_price",
                "invoice",
            ],
            &[],
            "delivery_id",
        )),
        "supplier-reputation" => Some((
            &[
                "reputation_id",
                "supplier_id",
                "component_id",
                "agent_id",
                "offered",
                "purchased",
                "reputation_ppm",
            ],
            &[],
            "reputation_id",
        )),
        "customer-rfq" => Some((
            &[
                "rfq_id",
                "product_id",
                "quantity",
                "due_day",
                "unit_reserve",
                "daily_penalty",
            ],
            &[],
            "rfq_id",
        )),
        "customer-bid" => Some((
            &[
                "bid_id",
                "agent_id",
                "rfq_id",
                "bid_day",
                "award_day",
                "unit_price",
            ],
            &[],
            "bid_id",
        )),
        "customer-order" => Some((
            &[
                "order_id",
                "rfq_id",
                "agent_id",
                "product_id",
                "quantity",
                "due_day",
                "unit_price",
                "daily_penalty",
                "penalty_count",
                "status",
                "payment_status",
                "delivery_day",
            ],
            &[],
            "order_id",
        )),
        "customer-settlement" => Some((
            &[
                "settlement_id",
                "order_id",
                "delivered",
                "payment",
                "penalty",
                "penalty_count",
                "cancelled",
            ],
            &[],
            "settlement_id",
        )),
        "inventory-item" => Some((
            &[
                "inventory_id",
                "agent_id",
                "inventory_kind",
                "item_id",
                "quantity",
            ],
            &[],
            "inventory_id",
        )),
        "schedule-result" => Some((
            &[
                "result_id",
                "agent_id",
                "schedule_kind",
                "line_id",
                "executed",
                "quantity",
            ],
            &["rejection"],
            "result_id",
        )),
        _ => None,
    }
}

fn entity_id_matches(entity_type: &str, entity_id: &str) -> bool {
    let prefix = format!("{entity_type}/");
    valid_engine_id(entity_id) && entity_id.starts_with(&prefix)
}

fn generic_entity_type(entity_type: &str) -> bool {
    matches!(
        entity_type,
        "agent-state"
            | "supplier-line-state"
            | "supplier-delivery"
            | "supplier-reputation"
            | "customer-settlement"
            | "inventory-item"
            | "schedule-result"
    )
}

fn generic_economic_kind(kind: &str) -> bool {
    !matches!(
        kind,
        "bank-interest-assessment" | "storage-assessment" | "customer-penalty"
    )
}

fn hashed_id(prefix: &str, fields: Vec<Value>) -> String {
    format!("{prefix}/{}", hex(&sha256(&Value::Array(fields).encode())))
}

fn economic_detail_fields(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "supplier-price" => Some(&["unit_price"]),
        "supplier-allocation" => Some(&["on_date_quantity"]),
        "supplier-production" => Some(&["actual_capacity"]),
        "supplier-inventory" => Some(&["before", "after"]),
        "supplier-refund" => Some(&["order_id", "undelivered_quantity", "amount"]),
        "customer-demand" => Some(&["segment", "q_nano", "trend_ppb", "next_ordinal"]),
        "customer-award" => Some(&["bid_id", "order_id"]),
        "customer-payment" => Some(&["settlement_id", "amount"]),
        "customer-penalty" => Some(&["settlement_id", "penalty_count", "amount"]),
        "customer-cancellation" => Some(&["settlement_id", "penalty_count"]),
        "factory-production" => Some(&[
            "agent_id",
            "line_id",
            "result_id",
            "product_id",
            "quantity",
            "cycles",
        ]),
        "factory-delivery" => Some(&[
            "agent_id",
            "line_id",
            "result_id",
            "order_id",
            "product_id",
            "quantity",
        ]),
        "factory-inventory" => Some(&[
            "agent_id",
            "inventory_id",
            "inventory_kind",
            "item_id",
            "before",
            "after",
        ]),
        "bank-interest-assessment" => Some(&[
            "agent_id",
            "opening_cash",
            "annual_rate_ppb",
            "amount",
            "direction",
        ]),
        "storage-assessment" => Some(&["agent_id", "inventory_value", "annual_rate_ppb", "amount"]),
        "command-rejection" => Some(&["agent_id", "command_id", "code", "field_path", "detail"]),
        _ => None,
    }
}

fn cause_array(fields: &[(String, Value)], context: &str) -> Result<Vec<String>, ConformanceError> {
    array(field(fields, "causes", context)?, context)?
        .iter()
        .map(|value| Ok(text(value, context)?.to_owned()))
        .collect()
}

/// Validates a complete RFC 8742 sequence produced by the 0.4 engine.
pub fn validate_event_log(input: &[u8]) -> Result<ConformanceReport, ConformanceError> {
    let mut offset = 0_usize;
    let mut expected_index = 0_u64;
    let mut previous_hash = [0_u8; 32];
    let mut open_phase: Option<(u64, String)> = None;
    let mut entities: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut origins: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut economic_count = 0_u64;
    let mut transaction_count = 0_u64;
    let mut entity_types = BTreeSet::new();
    let mut economic_kinds = BTreeSet::new();
    let mut entity_ordinals: BTreeMap<(String, u64, String), u64> = BTreeMap::new();
    let mut economic_ordinals: BTreeMap<(String, u64, String), u64> = BTreeMap::new();
    let mut active_permutation: Option<(String, u64, Vec<String>)> = None;

    while offset < input.len() {
        let (event, consumed) = decode_one(&input[offset..]).map_err(|error| {
            ConformanceError(format!("event {expected_index} CBOR at {offset}: {error}"))
        })?;
        if consumed == 0 || event.encode() != input[offset..offset + consumed] {
            return fail(format!("event {expected_index} is not canonical CBOR"));
        }
        offset += consumed;
        let event_fields = map(&event, "event")?;
        closed(
            event_fields,
            &[
                "schema_id",
                "event_index",
                "event_type",
                "game_id",
                "day",
                "phase",
                "causes",
                "visibility",
                "payload",
                "previous_event_hash",
                "event_hash",
            ],
            &["state_commitment"],
            "event",
        )?;
        if text(field(event_fields, "schema_id", "event")?, "event schema")? != EVENT_SCHEMA_ID {
            return fail(format!("event {expected_index} has the wrong schema"));
        }
        if unsigned(field(event_fields, "event_index", "event")?, "event index")? != expected_index
        {
            return fail(format!("event index mismatch at {expected_index}"));
        }
        if bytes(
            field(event_fields, "previous_event_hash", "event")?,
            "previous event hash",
        )? != previous_hash
        {
            return fail(format!("previous event hash mismatch at {expected_index}"));
        }
        let recorded_hash = bytes(field(event_fields, "event_hash", "event")?, "event hash")?;
        if recorded_hash.len() != 32 {
            return fail(format!("event hash length mismatch at {expected_index}"));
        }
        let unhashed = Value::Map(
            event_fields
                .iter()
                .filter(|(name, _)| name != "event_hash")
                .cloned()
                .collect(),
        );
        let actual_hash = sha256(&unhashed.encode());
        if recorded_hash != actual_hash {
            return fail(format!("event hash mismatch at {expected_index}"));
        }
        previous_hash.copy_from_slice(recorded_hash);

        let event_type = text(field(event_fields, "event_type", "event")?, "event type")?;
        let day = unsigned(field(event_fields, "day", "event")?, "event day")?;
        let phase = text(field(event_fields, "phase", "event")?, "event phase")?;
        match event_type {
            "phase-opened" => {
                if open_phase.is_some() {
                    return fail(format!("nested phase at event {expected_index}"));
                }
                open_phase = Some((day, phase.to_owned()));
            }
            "phase-closed" => {
                if open_phase.as_ref() != Some(&(day, phase.to_owned())) {
                    return fail(format!("phase close mismatch at event {expected_index}"));
                }
                open_phase = None;
            }
            _ if open_phase.as_ref() != Some(&(day, phase.to_owned())) => {
                return fail(format!("event {expected_index} is outside its open phase"));
            }
            _ => {}
        }

        let payload = map(field(event_fields, "payload", "event")?, "event payload")?;
        if event_type != "random-draw" && active_permutation.is_some() {
            return fail(format!(
                "uniform permutation is interrupted at event {expected_index}"
            ));
        }
        match event_type {
            "random-draw" => {
                closed(
                    payload,
                    &["stream_name", "ordinal", "raw_u64", "result"],
                    &[],
                    "random event",
                )?;
                let stream = text(
                    field(payload, "stream_name", "random event")?,
                    "random stream",
                )?;
                unsigned(field(payload, "ordinal", "random event")?, "draw ordinal")?;
                unsigned(field(payload, "raw_u64", "random event")?, "raw draw")?;
                let result = field(payload, "result", "random event")?;
                if let Value::Map(selection) = result {
                    closed(
                        selection,
                        &["candidates", "selected_index", "winner"],
                        &[],
                        "uniform selection",
                    )?;
                    let candidates = array(
                        field(selection, "candidates", "uniform selection")?,
                        "uniform candidates",
                    )?
                    .iter()
                    .map(|candidate| {
                        let candidate = text(candidate, "uniform candidate")?;
                        if !valid_engine_id(candidate) {
                            return fail(format!("invalid uniform candidate {candidate}"));
                        }
                        Ok(candidate.to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                    if candidates.is_empty()
                        || candidates
                            .windows(2)
                            .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
                    {
                        return fail("uniform candidates are not non-empty, sorted, and unique");
                    }
                    let selected = usize::try_from(unsigned(
                        field(selection, "selected_index", "uniform selection")?,
                        "uniform selected index",
                    )?)
                    .map_err(|_| ConformanceError("uniform selected index is too large".into()))?;
                    let winner = text(
                        field(selection, "winner", "uniform selection")?,
                        "uniform winner",
                    )?;
                    if candidates.get(selected).map(String::as_str) != Some(winner) {
                        return fail("uniform winner does not match selected index");
                    }
                    let Some((base, rank)) = stream.rsplit_once("/rank/") else {
                        return fail("uniform selection stream has no rank suffix");
                    };
                    if !base.starts_with("symmetry/") {
                        return fail("uniform selection stream is outside symmetry namespace");
                    }
                    let rank = rank
                        .parse::<u64>()
                        .map_err(|_| ConformanceError("uniform rank is not an integer".into()))?;
                    match &active_permutation {
                        None if rank == 0 => {}
                        Some((expected_base, expected_rank, expected_candidates))
                            if expected_base == base
                                && *expected_rank == rank
                                && *expected_candidates == candidates => {}
                        _ => return fail("uniform permutation rank or candidate chain mismatch"),
                    }
                    let mut remaining = candidates;
                    remaining.remove(selected);
                    active_permutation =
                        (remaining.len() > 1).then(|| (base.to_owned(), rank + 1, remaining));
                } else if !matches!(result, Value::U64(_) | Value::I64(_) | Value::Text(_)) {
                    return fail("random result is outside the closed scalar/selection union");
                } else if let Some((base, rank, _)) = &active_permutation {
                    if stream != format!("{base}/rank/{rank}") {
                        return fail("rejected uniform draw interrupts permutation chain");
                    }
                } else if stream.starts_with("symmetry/") && !stream.ends_with("/rank/0") {
                    return fail("orphaned rejected uniform draw");
                }
            }
            "entity-changed" => {
                closed(
                    payload,
                    &[
                        "entity_type",
                        "entity_id",
                        "operation",
                        "value",
                        "value_sha256",
                    ],
                    &[],
                    "entity event",
                )?;
                let entity_type = text(field(payload, "entity_type", "entity")?, "entity type")?;
                let entity_id = text(field(payload, "entity_id", "entity")?, "entity id")?;
                let operation = text(field(payload, "operation", "entity")?, "entity operation")?;
                let Some((required, optional, identity_field)) = entity_value_contract(entity_type)
                else {
                    return fail(format!("unregistered entity type {entity_type}"));
                };
                entity_types.insert(entity_type.to_owned());
                if !entity_id_matches(entity_type, entity_id) {
                    return fail(format!("invalid {entity_type} id {entity_id}"));
                }
                let value = embedded_value(payload, "entity")?;
                let value_fields = map(&value, "entity value")?;
                closed(value_fields, required, optional, "entity value")?;
                if text(
                    field(value_fields, identity_field, "entity value")?,
                    identity_field,
                )? != entity_id
                {
                    return fail(format!("entity value identity mismatch for {entity_id}"));
                }
                if operation == "created" {
                    let expected_id = if generic_entity_type(entity_type) {
                        let key = (entity_type.to_owned(), day, phase.to_owned());
                        let ordinal = entity_ordinals.entry(key).or_default();
                        let expected = format!("{entity_type}/{day}/{phase}/{ordinal}");
                        *ordinal += 1;
                        expected
                    } else {
                        match entity_type {
                            "supplier-rfq" => hashed_id(
                                "supplier-rfq",
                                vec![
                                    field(value_fields, "agent_id", "supplier RFQ")?.clone(),
                                    field(value_fields, "supplier_id", "supplier RFQ")?.clone(),
                                    field(value_fields, "client_rfq_id", "supplier RFQ")?.clone(),
                                ],
                            ),
                            "supplier-offer" => {
                                let valid_day = unsigned(
                                    field(value_fields, "valid_day", "supplier offer")?,
                                    "supplier offer valid day",
                                )?;
                                hashed_id(
                                    "supplier-offer",
                                    vec![
                                        field(value_fields, "rfq_id", "supplier offer")?.clone(),
                                        Value::U64(valid_day.saturating_sub(1)),
                                        field(value_fields, "kind", "supplier offer")?.clone(),
                                        field(value_fields, "due_day", "supplier offer")?.clone(),
                                        field(value_fields, "quantity", "supplier offer")?.clone(),
                                        field(value_fields, "unit_price", "supplier offer")?
                                            .clone(),
                                    ],
                                )
                            }
                            "supplier-order" => hashed_id(
                                "supplier-order",
                                vec![
                                    field(value_fields, "agent_id", "supplier order")?.clone(),
                                    field(value_fields, "offer_id", "supplier order")?.clone(),
                                ],
                            ),
                            "customer-bid" => hashed_id(
                                "customer-bid",
                                vec![
                                    field(value_fields, "agent_id", "customer bid")?.clone(),
                                    field(value_fields, "rfq_id", "customer bid")?.clone(),
                                    field(value_fields, "bid_day", "customer bid")?.clone(),
                                ],
                            ),
                            "customer-order" => format!(
                                "customer-order/{}",
                                text(
                                    field(value_fields, "rfq_id", "customer order")?,
                                    "customer order RFQ",
                                )?
                            ),
                            "customer-rfq" => {
                                let parts = entity_id.split('/').collect::<Vec<_>>();
                                if parts.len() != 4
                                    || parts[0] != "customer-rfq"
                                    || parts[1] != day.to_string()
                                    || !matches!(parts[2], "low" | "mid" | "high")
                                    || parts[3].parse::<u64>().is_err()
                                {
                                    return fail(format!("invalid customer RFQ id {entity_id}"));
                                }
                                entity_id.to_owned()
                            }
                            _ => return fail(format!("missing id rule for {entity_type}")),
                        }
                    };
                    if entity_id != expected_id {
                        return fail(format!(
                            "entity id differs from creation rule: {entity_id} != {expected_id}"
                        ));
                    }
                }
                match operation {
                    "created" => {
                        if entities.contains_key(entity_id)
                            || origins.insert(entity_id.into(), "entity").is_some()
                        {
                            return fail(format!(
                                "duplicate or cross-origin entity id {entity_id}"
                            ));
                        }
                        entities.insert(entity_id.into(), (entity_type.into(), true));
                    }
                    "updated" | "removed" => {
                        let Some((prior_type, active)) = entities.get_mut(entity_id) else {
                            return fail(format!("{operation} before created for {entity_id}"));
                        };
                        if prior_type != entity_type || !*active {
                            return fail(format!("invalid {operation} lifecycle for {entity_id}"));
                        }
                        if operation == "removed" {
                            *active = false;
                        }
                    }
                    _ => return fail(format!("invalid entity operation {operation}")),
                }
            }
            "economic-event" => {
                closed(
                    payload,
                    &["economic_id", "kind", "value", "value_sha256"],
                    &["subject_id"],
                    "economic event",
                )?;
                let economic_id = text(field(payload, "economic_id", "economic")?, "economic id")?;
                let kind = text(field(payload, "kind", "economic")?, "economic kind")?;
                let Some(detail_fields) = economic_detail_fields(kind) else {
                    return fail(format!("unregistered economic kind {kind}"));
                };
                economic_kinds.insert(kind.to_owned());
                if !valid_engine_id(economic_id)
                    || origins.insert(economic_id.into(), "economic").is_some()
                {
                    return fail(format!("invalid or reused economic id {economic_id}"));
                }
                let detail = embedded_value(payload, "economic")?;
                let detail_map = map(&detail, "economic detail")?;
                closed(detail_map, detail_fields, &[], "economic detail")?;
                let expected_id = if generic_economic_kind(kind) {
                    let key = (kind.to_owned(), day, phase.to_owned());
                    let ordinal = economic_ordinals.entry(key).or_default();
                    let expected = format!("{kind}/{day}/{phase}/{ordinal}");
                    *ordinal += 1;
                    expected
                } else if kind == "bank-interest-assessment" {
                    format!(
                        "finance-assessment/{day}/{}",
                        text(
                            field(detail_map, "agent_id", "finance assessment")?,
                            "finance agent",
                        )?
                    )
                } else if kind == "storage-assessment" {
                    format!(
                        "storage-assessment/{day}/{}",
                        text(
                            field(detail_map, "agent_id", "storage assessment")?,
                            "storage agent",
                        )?
                    )
                } else {
                    let subject = optional_field(payload, "subject_id")
                        .ok_or_else(|| ConformanceError("penalty subject is absent".into()))?;
                    format!(
                        "customer-penalty/{}/{}",
                        text(subject, "penalty subject")?,
                        unsigned(
                            field(detail_map, "penalty_count", "penalty")?,
                            "penalty count",
                        )?
                    )
                };
                if economic_id != expected_id {
                    return fail(format!(
                        "economic id differs from creation rule: {economic_id} != {expected_id}"
                    ));
                }
                if cause_array(event_fields, "economic event")? != [economic_id] {
                    return fail(format!("economic causes do not bind {economic_id}"));
                }
                if let Some(subject) = optional_field(payload, "subject_id") {
                    let subject = text(subject, "economic subject")?;
                    if !entities.get(subject).is_some_and(|(_, active)| *active) {
                        return fail(format!(
                            "economic subject is not an active entity: {subject}"
                        ));
                    }
                }
                economic_count += 1;
            }
            "ledger-transferred" => {
                closed(payload, &["transaction"], &[], "ledger event")?;
                let transaction = map(field(payload, "transaction", "ledger")?, "transaction")?;
                closed(
                    transaction,
                    &[
                        "transaction_id",
                        "day",
                        "phase",
                        "source_account",
                        "destination_account",
                        "amount",
                        "reason",
                        "cause_id",
                    ],
                    &[],
                    "transaction",
                )?;
                let transaction_id = text(
                    field(transaction, "transaction_id", "transaction")?,
                    "transaction id",
                )?;
                let transaction_day =
                    unsigned(field(transaction, "day", "transaction")?, "transaction day")?;
                let transaction_phase = text(
                    field(transaction, "phase", "transaction")?,
                    "transaction phase",
                )?;
                let source = text(
                    field(transaction, "source_account", "transaction")?,
                    "source account",
                )?;
                let destination = text(
                    field(transaction, "destination_account", "transaction")?,
                    "destination account",
                )?;
                let reason = text(field(transaction, "reason", "transaction")?, "reason")?;
                let cause_id = text(field(transaction, "cause_id", "transaction")?, "cause id")?;
                if unsigned(field(transaction, "amount", "transaction")?, "amount")? == 0 {
                    return fail(format!("zero ledger amount for {transaction_id}"));
                }
                let source_agent = source.strip_prefix("agent-cash/");
                let destination_agent = destination.strip_prefix("agent-cash/");
                let agent_id = match (source_agent, destination_agent) {
                    (Some(agent), None) | (None, Some(agent)) => agent,
                    _ => {
                        return fail(format!(
                            "ledger endpoints do not identify one agent: {transaction_id}"
                        ));
                    }
                };
                let shared = if source_agent.is_none() {
                    source
                } else {
                    destination
                };
                if !matches!(
                    shared,
                    "bank-interest" | "supplier-market" | "customer-market" | "storage-market"
                ) {
                    return fail(format!("unregistered shared account {shared}"));
                }
                if !origins.contains_key(cause_id) || cause_id == transaction_id {
                    return fail(format!("unbound ledger cause {cause_id}"));
                }
                let expected_id = format!(
                    "ledger-transaction/{}",
                    hex(&sha256(
                        &Value::Array(vec![
                            Value::U64(transaction_day),
                            Value::Text(transaction_phase.into()),
                            Value::Text(agent_id.into()),
                            Value::Text(reason.into()),
                            Value::Text(cause_id.into()),
                        ])
                        .encode(),
                    ))
                );
                if transaction_id != expected_id
                    || transaction_day != day
                    || transaction_phase != phase
                    || origins.insert(transaction_id.into(), "ledger").is_some()
                {
                    return fail(format!(
                        "invalid or reused ledger transaction {transaction_id}"
                    ));
                }
                if cause_array(event_fields, "ledger event")? != [cause_id] {
                    return fail(format!("ledger event cause mismatch for {transaction_id}"));
                }
                transaction_count += 1;
            }
            _ => {}
        }
        expected_index += 1;
    }
    if open_phase.is_some() {
        return fail("event sequence ends with an open phase");
    }
    if active_permutation.is_some() {
        return fail("event sequence ends inside a uniform permutation");
    }
    Ok(ConformanceReport {
        event_count: expected_index,
        entity_id_count: u64::try_from(entities.len()).unwrap(),
        economic_id_count: economic_count,
        ledger_transaction_count: transaction_count,
        entity_types,
        economic_kinds,
    })
}
