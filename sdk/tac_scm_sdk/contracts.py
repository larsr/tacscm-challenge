"""Immutable logical records for the complete TAC SCM 2026 agent boundary."""

from __future__ import annotations

from dataclasses import dataclass, field, is_dataclass
from collections.abc import Mapping
import re
from types import MappingProxyType
from typing import Any, Protocol, runtime_checkable


EXTERNAL_ID = re.compile(r"^[a-z0-9][a-z0-9._-]{0,31}$")
ENGINE_ID = re.compile(r"^[a-z0-9][a-z0-9._/-]{0,95}$")
MESSAGE_SCHEMA_ID = "tac-scm-message/2"
MESSAGE_SCHEMA_VERSION = (2, 0, 0)
DEFAULT_CAUSES = frozenset(
    {"agent-submission", "missing", "timeout", "process-exit", "resource-limit", "transport-error"}
)
ROLES = frozenset({"coordinator", "engine", "manufacturer", "supplier", "customer", "factory", "bank", "runner", "visualizer"})
PHASES = frozenset(
    {
        "plan", "setup", "finance-open", "supplier-fulfillment", "factory-execution",
        "customer-settlement", "inventory-close", "observation", "action-window",
        "action-commit", "final-settlement", "terminated", "aborted",
    }
)


class ContractError(ValueError):
    """A logical record failed closed validation."""


def _external(value: str, name: str = "external id") -> str:
    if not isinstance(value, str) or EXTERNAL_ID.fullmatch(value) is None:
        raise ContractError(f"{name} must match [a-z0-9][a-z0-9._-]{{0,31}}")
    return value


def _engine(value: str, name: str = "engine id") -> str:
    if not isinstance(value, str) or ENGINE_ID.fullmatch(value) is None:
        raise ContractError(f"{name} must be lowercase ASCII and at most 96 bytes")
    return value


def _bounded(value: int, low: int, high: int, name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise ContractError(f"{name} must be in {low}..{high}")
    return value


def _digest(value: bytes, name: str = "digest") -> bytes:
    if not isinstance(value, bytes) or len(value) != 32:
        raise ContractError(f"{name} must contain exactly 32 bytes")
    return value


def _closed(
    value: Any,
    required: set[str],
    optional: set[str] | None = None,
    name: str = "record",
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{name} must be a map")
    keys = set(value)
    missing = required - keys
    unknown = keys - required - (optional or set())
    if missing:
        raise ContractError(f"{name} missing fields: {', '.join(sorted(missing))}")
    if unknown:
        raise ContractError(f"{name} unknown fields: {', '.join(sorted(unknown))}")
    return value


def _tuple(value: Any, parser: Any, name: str) -> tuple[Any, ...]:
    if not isinstance(value, list):
        raise ContractError(f"{name} must be an array")
    return tuple(parser(item) for item in value)


def _freeze(value: Any) -> Any:
    if isinstance(value, dict):
        return MappingProxyType({key: _freeze(item) for key, item in value.items()})
    if isinstance(value, list):
        return tuple(_freeze(item) for item in value)
    return value


@dataclass(frozen=True, slots=True)
class ArtifactReference:
    id: str
    revision: int
    sha256: bytes

    def __post_init__(self) -> None:
        if not isinstance(self.id, str) or not 1 <= len(self.id.encode()) <= 64:
            raise ContractError("schema id must contain 1..64 UTF-8 bytes")
        _bounded(self.revision, 0, 0xFFFF_FFFF, "artifact revision")
        _digest(self.sha256)


@dataclass(frozen=True, slots=True)
class Component:
    id: str
    base_microcredits: int
    suppliers: tuple[str, ...]

    def __post_init__(self) -> None:
        _external(self.id, "component id")
        _bounded(self.base_microcredits, -(1 << 63), (1 << 63) - 1, "base price")
        if not 1 <= len(self.suppliers) <= 2:
            raise ContractError("component suppliers must contain one or two ids")
        for value in self.suppliers:
            _external(value, "supplier id")


@dataclass(frozen=True, slots=True)
class Product:
    id: str
    components: tuple[str, ...]
    cycles: int
    segment: str

    def __post_init__(self) -> None:
        _external(self.id, "product id")
        if len(self.components) != 4:
            raise ContractError("product BOM must contain four component ids")
        for value in self.components:
            _external(value, "component id")
        _bounded(self.cycles, 4, 7, "cycles")
        if self.segment not in {"low", "mid", "high"}:
            raise ContractError("unknown product segment")


@dataclass(frozen=True, slots=True)
class Supplier:
    id: str
    components: tuple[str, ...]

    def __post_init__(self) -> None:
        _external(self.id, "supplier id")
        if not 1 <= len(self.components) <= 4:
            raise ContractError("supplier components must contain 1..4 ids")


@dataclass(frozen=True, slots=True)
class Catalog:
    components: tuple[Component, ...]
    products: tuple[Product, ...]
    suppliers: tuple[Supplier, ...]

    def __post_init__(self) -> None:
        if (len(self.components), len(self.products), len(self.suppliers)) != (10, 16, 8):
            raise ContractError("catalog must contain 10 components, 16 products, and 8 suppliers")


@dataclass(frozen=True, slots=True)
class GameBootstrap:
    game_definition_id: str
    schema: ArtifactReference
    profile: ArtifactReference
    modules: tuple[ArtifactReference, ...]
    agent_id: str
    agent_slot: int
    roster: tuple[str, ...]
    semantic_configuration: Mapping[str, Any]
    catalog: Catalog
    latest_committed_day: int | None
    type: str = field(default="game-bootstrap", init=False)

    def __post_init__(self) -> None:
        _engine(self.game_definition_id)
        _external(self.agent_id, "agent id")
        _bounded(self.agent_slot, 0, 5, "agent slot")
        if len(self.modules) != 3 or len(self.roster) != 6:
            raise ContractError("bootstrap requires three modules and six roster entries")
        if self.roster[self.agent_slot] != self.agent_id:
            raise ContractError("agent slot does not bind bootstrap agent")
        if self.latest_committed_day is not None:
            _bounded(self.latest_committed_day, 0, 219, "latest committed day")


@dataclass(frozen=True, slots=True)
class Rejection:
    code: str
    path: str
    detail: str


@dataclass(frozen=True, slots=True)
class CommandResult:
    command_id: str
    accepted: bool
    rejection: Rejection | None = None

    def __post_init__(self) -> None:
        _external(self.command_id, "command id")
        if self.accepted == (self.rejection is not None):
            raise ContractError("accepted command has no rejection; rejected command has one")


@dataclass(frozen=True, slots=True)
class InventoryItem:
    inventory_id: str
    agent_id: str
    inventory_kind: str
    item_id: str
    quantity: int

    def __post_init__(self) -> None:
        _external(self.item_id, "inventory item id")
        _bounded(self.quantity, 0, 0xFFFF_FFFF, "quantity")


@dataclass(frozen=True, slots=True)
class LedgerTransaction:
    transaction_id: str
    day: int
    phase: str
    source_account: str
    destination_account: str
    amount: int
    reason: str
    cause_id: str


@dataclass(frozen=True, slots=True)
class ScheduleResult:
    result_id: str
    agent_id: str
    schedule_kind: str
    line_id: str
    executed: bool
    quantity: int
    rejection: Rejection | None = None


@dataclass(frozen=True, slots=True)
class SupplierRfq:
    rfq_id: str
    agent_id: str
    client_rfq_id: str
    supplier_id: str
    component_id: str
    quantity: int
    due_day: int
    max_unit_price: int | None


@dataclass(frozen=True, slots=True)
class SupplierOffer:
    offer_id: str
    alternative_group_id: str
    rfq_id: str
    agent_id: str
    supplier_id: str
    component_id: str
    kind: str
    quantity: int
    due_day: int
    unit_price: int
    valid_day: int
    reputation_exposure: int
    no_offer_reason: str | None


@dataclass(frozen=True, slots=True)
class SupplierOrder:
    order_id: str
    offer_id: str
    agent_id: str
    supplier_id: str
    component_id: str
    quantity: int
    due_day: int
    unit_price: int
    down_payment: int
    acceptance_ordinal: int
    delivered_quantity: int


@dataclass(frozen=True, slots=True)
class SupplierDelivery:
    delivery_id: str
    order_id: str
    component_id: str
    quantity: int
    unit_price: int
    invoice: int


@dataclass(frozen=True, slots=True)
class SupplierReputation:
    reputation_id: str
    supplier_id: str
    component_id: str
    agent_id: str
    offered: int
    purchased: int
    reputation_ppm: int


@dataclass(frozen=True, slots=True)
class CustomerRfq:
    rfq_id: str
    product_id: str
    quantity: int
    due_day: int
    unit_reserve: int
    daily_penalty: int


@dataclass(frozen=True, slots=True)
class CustomerOrder:
    order_id: str
    rfq_id: str
    agent_id: str
    product_id: str
    quantity: int
    due_day: int
    unit_price: int
    daily_penalty: int
    penalty_count: int
    status: str
    payment_status: str
    delivery_day: int | None


@dataclass(frozen=True, slots=True)
class CustomerSettlement:
    settlement_id: str
    order_id: str
    delivered: bool
    payment: int
    penalty: int
    penalty_count: int
    cancelled: bool


@dataclass(frozen=True, slots=True)
class ActionDisposition:
    day: int
    action_digest: bytes
    cause: str
    revision: int | None = None


@dataclass(frozen=True, slots=True)
class DefaultRecord:
    day: int
    cause: str


@dataclass(frozen=True, slots=True)
class PriceRange:
    product_id: str
    minimum: int | None
    maximum: int | None


@dataclass(frozen=True, slots=True)
class DailyPriceReport:
    day: int
    products: tuple[PriceRange, ...]


@dataclass(frozen=True, slots=True)
class ProductMarketRow:
    product_id: str
    rfq_count: int
    rfq_quantity: int
    order_count: int
    order_quantity: int
    minimum_price: int | None
    maximum_price: int | None
    mean_price: int | None


@dataclass(frozen=True, slots=True)
class ComponentMarketRow:
    component_id: str
    ordered_quantity: int
    delivered_quantity: int
    mean_unit_price: int | None


@dataclass(frozen=True, slots=True)
class CapacityMarketRow:
    supplier_id: str
    component_id: str
    mean_capacity_nano: int | None


@dataclass(frozen=True, slots=True)
class MarketReport:
    start_day: int
    end_day: int
    products: tuple[ProductMarketRow, ...]
    components: tuple[ComponentMarketRow, ...]
    supplier_lines: tuple[CapacityMarketRow, ...]


@dataclass(frozen=True, slots=True)
class DayObservation:
    agent_id: str
    observation_id: str
    day: int
    state_commitment: bytes
    cash_microcredits: int
    event_cursor: int = 0
    ledger_since_previous: tuple[LedgerTransaction, ...] = ()
    component_inventory: tuple[InventoryItem, ...] = ()
    pc_inventory: tuple[InventoryItem, ...] = ()
    pending_inventory: tuple[InventoryItem, ...] = ()
    production_results: tuple[ScheduleResult, ...] = ()
    delivery_results: tuple[ScheduleResult, ...] = ()
    supplier_rfqs: tuple[SupplierRfq, ...] = ()
    supplier_offers: tuple[SupplierOffer, ...] = ()
    supplier_orders: tuple[SupplierOrder, ...] = ()
    supplier_deliveries: tuple[SupplierDelivery, ...] = ()
    supplier_reputations: tuple[SupplierReputation, ...] = ()
    customer_rfqs: tuple[CustomerRfq, ...] = ()
    new_customer_orders: tuple[CustomerOrder, ...] = ()
    active_customer_orders: tuple[CustomerOrder, ...] = ()
    customer_settlements: tuple[CustomerSettlement, ...] = ()
    previous_action: ActionDisposition | None = None
    daily_price_report: DailyPriceReport | None = None
    market_report: MarketReport | None = None
    default_history: tuple[DefaultRecord, ...] = ()
    type: str = field(default="day-observation", init=False)

    def __post_init__(self) -> None:
        _external(self.agent_id, "agent id")
        _engine(self.observation_id, "observation id")
        _bounded(self.day, 0, 219, "day")
        _digest(self.state_commitment, "state commitment")
        _bounded(self.event_cursor, 0, 0xFFFF_FFFF_FFFF_FFFF, "event cursor")
        _bounded(self.cash_microcredits, -(1 << 63), (1 << 63) - 1, "cash")

    def inventory(self, item_id: str, *, pending: bool = False) -> int:
        """Return an item's observed quantity without exposing engine state."""

        records = self.pending_inventory if pending else self.component_inventory + self.pc_inventory
        return sum(item.quantity for item in records if item.item_id == item_id)


@dataclass(frozen=True, slots=True)
class CustomerOfferCommand:
    command_id: str
    rfq_id: str
    unit_price: int

    def __post_init__(self) -> None:
        _external(self.command_id, "command id")
        _engine(self.rfq_id, "customer RFQ id")
        _bounded(self.unit_price, -(1 << 63), (1 << 63) - 1, "unit price")


@dataclass(frozen=True, slots=True)
class SupplierRfqCommand:
    command_id: str
    rfq_id: str
    supplier_id: str
    component_id: str
    quantity: int
    due_day: int
    max_unit_price: int | None

    def __post_init__(self) -> None:
        _external(self.command_id, "command id")
        _external(self.rfq_id, "client RFQ id")
        _external(self.supplier_id, "supplier id")
        _external(self.component_id, "component id")
        _bounded(self.quantity, 0, 0xFFFF_FFFF, "quantity")
        _bounded(self.due_day, 0, 219, "due day")
        if self.max_unit_price is not None:
            _bounded(self.max_unit_price, -(1 << 63), (1 << 63) - 1, "maximum unit price")


@dataclass(frozen=True, slots=True)
class SupplierOrderCommand:
    command_id: str
    offer_id: str

    def __post_init__(self) -> None:
        _external(self.command_id, "command id")
        _engine(self.offer_id, "supplier offer id")


@dataclass(frozen=True, slots=True)
class ProductionLine:
    line_id: str
    priority: int
    product_id: str
    quantity: int

    def __post_init__(self) -> None:
        _external(self.line_id, "line id")
        _bounded(self.priority, 0, 0xFFFF_FFFF, "priority")
        _external(self.product_id, "product id")
        _bounded(self.quantity, 1, 0xFFFF_FFFF, "quantity")


@dataclass(frozen=True, slots=True)
class DeliveryLine:
    line_id: str
    priority: int
    customer_order_id: str
    quantity: int

    def __post_init__(self) -> None:
        _external(self.line_id, "line id")
        _bounded(self.priority, 0, 0xFFFF_FFFF, "priority")
        _engine(self.customer_order_id, "customer order id")
        _bounded(self.quantity, 1, 0xFFFF_FFFF, "quantity")


@dataclass(frozen=True, slots=True)
class DayActionSet:
    agent_id: str
    observation_id: str
    day: int
    revision: int
    customer_offers: tuple[CustomerOfferCommand, ...] = ()
    supplier_rfqs: tuple[SupplierRfqCommand, ...] = ()
    supplier_orders: tuple[SupplierOrderCommand, ...] = ()
    production_schedule: tuple[ProductionLine, ...] = ()
    delivery_schedule: tuple[DeliveryLine, ...] = ()
    type: str = field(default="day-action-set", init=False)

    def __post_init__(self) -> None:
        _external(self.agent_id, "agent id")
        _engine(self.observation_id, "observation id")
        _bounded(self.day, 0, 219, "day")
        _bounded(self.revision, 1, 0xFFFF_FFFF, "revision")
        if len(self.production_schedule) > 256 or len(self.delivery_schedule) > 4096:
            raise ContractError("factory schedule cardinality exceeded")
        command_ids = [item.command_id for items in (self.customer_offers, self.supplier_rfqs, self.supplier_orders) for item in items]
        line_ids = [item.line_id for items in (self.production_schedule, self.delivery_schedule) for item in items]
        # One namespace over all commands and lines; the engine treats a
        # collision, or a repeated (supplier, rfq id) pair, as fatal.
        if len(command_ids) + len(line_ids) != len(set(command_ids + line_ids)):
            raise ContractError("command and line ids must be unique within the action set")
        rfq_keys = [(rfq.supplier_id, rfq.rfq_id) for rfq in self.supplier_rfqs]
        if len(rfq_keys) != len(set(rfq_keys)):
            raise ContractError("supplier RFQ ids must be unique per supplier within the action set")
        # At most five positive RFQs and one zero-quantity probe per
        # supplier-component pair and day (tac-scm-2026-draft3).
        counts: dict[tuple[str, str], tuple[int, int]] = {}
        for rfq in self.supplier_rfqs:
            pair = (rfq.supplier_id, rfq.component_id)
            positive, probes = counts.get(pair, (0, 0))
            counts[pair] = (positive + (rfq.quantity > 0), probes + (rfq.quantity == 0))
        if any(positive > 5 or probes > 1 for positive, probes in counts.values()):
            raise ContractError("supplier RFQs exceed five positive requests or one probe per supplier-component pair")


@dataclass(frozen=True, slots=True)
class ActionReceipt:
    day: int
    revision: int | None
    action_digest: bytes | None
    receipt_ordinal: int
    candidate_valid: bool
    errors: tuple[Rejection, ...]
    type: str = field(default="action-receipt", init=False)

    def __post_init__(self) -> None:
        _bounded(self.day, 0, 219, "day")
        _bounded(self.receipt_ordinal, 0, 0xFFFF_FFFF_FFFF_FFFF, "receipt ordinal")
        if self.revision is not None:
            _bounded(self.revision, 1, 0xFFFF_FFFF, "revision")
        if self.action_digest is not None:
            _digest(self.action_digest, "action digest")
        if self.candidate_valid != (self.revision is not None and self.action_digest is not None and not self.errors):
            raise ContractError("candidate validity is inconsistent with receipt fields")


@dataclass(frozen=True, slots=True)
class ActionCommitted:
    day: int
    action_digest: bytes
    cause: str
    command_results: tuple[CommandResult, ...]
    type: str = field(default="action-committed", init=False)

    def __post_init__(self) -> None:
        _bounded(self.day, 0, 219, "day")
        _digest(self.action_digest, "action digest")
        if self.cause not in DEFAULT_CAUSES:
            raise ContractError("unknown action commit cause")


@dataclass(frozen=True, slots=True)
class Score:
    agent_id: str
    cash_microcredits: int


@dataclass(frozen=True, slots=True)
class GameFinished:
    status: str
    scores: tuple[Score, ...] | None
    final_state_commitment: bytes
    abort_reason: str | None = None
    type: str = field(default="game-finished", init=False)

    def __post_init__(self) -> None:
        _digest(self.final_state_commitment, "final state commitment")
        if self.status == "completed":
            if self.scores is None or len(self.scores) != 6 or self.abort_reason is not None:
                raise ContractError("completed game requires six scores and no abort reason")
        elif self.status == "aborted":
            if self.scores is not None or self.abort_reason is None:
                raise ContractError("aborted game requires null scores and an abort reason")
        else:
            raise ContractError("unknown game-finished status")


Payload = GameBootstrap | DayObservation | DayActionSet | ActionReceipt | ActionCommitted | GameFinished


@dataclass(frozen=True, slots=True)
class Message:
    schema_id: str
    schema_version: tuple[int, int, int]
    game_id: str
    message_id: str
    sender_role: str
    sender_id: str
    recipient: str
    phase: str
    sender_sequence: int
    payload_digest: bytes
    payload: Payload
    game_day: int | None = None
    causation_id: str | None = None
    final_submission: bool | None = None

    def __post_init__(self) -> None:
        if self.schema_id != MESSAGE_SCHEMA_ID or self.schema_version != MESSAGE_SCHEMA_VERSION:
            raise ContractError("unsupported message schema")
        _engine(self.game_id, "game id")
        _engine(self.message_id, "message id")
        if self.sender_role not in ROLES:
            raise ContractError("unknown sender role")
        if self.phase not in PHASES:
            raise ContractError("unknown message phase")
        _bounded(self.sender_sequence, 0, 0xFFFF_FFFF_FFFF_FFFF, "sender sequence")
        _digest(self.payload_digest, "payload digest")
        if self.final_submission is not None and not isinstance(self.final_submission, bool):
            raise ContractError("final_submission must be a boolean")
        if self.final_submission is not None and not isinstance(self.payload, DayActionSet):
            raise ContractError("final_submission is only valid for day actions")
        if isinstance(self.payload, GameFinished):
            expected = ("engine", "terminated" if self.payload.status == "completed" else "aborted")
        else:
            expected = {
                GameBootstrap: ("runner", "setup"),
                DayObservation: ("engine", "observation"),
                DayActionSet: ("manufacturer", "action-window"),
                ActionReceipt: ("runner", "action-window"),
                ActionCommitted: ("engine", "action-commit"),
            }[type(self.payload)]
        if (self.sender_role, self.phase) != expected:
            raise ContractError("payload is forbidden for sender role or phase")
        payload_day = getattr(self.payload, "day", None)
        if payload_day is not None and self.game_day != payload_day:
            raise ContractError("envelope day does not bind payload day")


@runtime_checkable
class Agent(Protocol):
    """A policy consumes only an immutable normative observation."""

    def act(self, observation: DayObservation) -> DayActionSet:
        """Return the complete replacement for one action window."""

        ...


def empty_action(observation: DayObservation, *, revision: int = 1) -> DayActionSet:
    return DayActionSet(observation.agent_id, observation.observation_id, observation.day, revision)


def _record(
    value: dict[str, Any],
    cls: Any,
    required: set[str],
    optional: set[str] | None = None,
) -> Any:
    value = _closed(value, required, optional, cls.__name__)
    return cls(**value)


def _artifact(value: Any) -> ArtifactReference:
    return _record(value, ArtifactReference, {"id", "revision", "sha256"})


def _rejection(value: Any) -> Rejection:
    return _record(value, Rejection, {"code", "path", "detail"})


def _inventory(value: Any) -> InventoryItem:
    return _record(value, InventoryItem, {"inventory_id", "agent_id", "inventory_kind", "item_id", "quantity"})


def _schedule_result(value: Any) -> ScheduleResult:
    value = _closed(value, {"result_id", "agent_id", "schedule_kind", "line_id", "executed", "quantity"}, {"rejection"}, "schedule-result")
    return ScheduleResult(value["result_id"], value["agent_id"], value["schedule_kind"], value["line_id"], value["executed"], value["quantity"], _rejection(value["rejection"]) if "rejection" in value else None)


def _plain(cls: Any, required: set[str]):
    return lambda value: _record(value, cls, required)


_ledger = _plain(LedgerTransaction, {"transaction_id", "day", "phase", "source_account", "destination_account", "amount", "reason", "cause_id"})
_supplier_rfq = _plain(SupplierRfq, {"rfq_id", "agent_id", "client_rfq_id", "supplier_id", "component_id", "quantity", "due_day", "max_unit_price"})
_supplier_offer = _plain(SupplierOffer, {"offer_id", "alternative_group_id", "rfq_id", "agent_id", "supplier_id", "component_id", "kind", "quantity", "due_day", "unit_price", "valid_day", "reputation_exposure", "no_offer_reason"})
_supplier_order = _plain(SupplierOrder, {"order_id", "offer_id", "agent_id", "supplier_id", "component_id", "quantity", "due_day", "unit_price", "down_payment", "acceptance_ordinal", "delivered_quantity"})
_supplier_delivery = _plain(SupplierDelivery, {"delivery_id", "order_id", "component_id", "quantity", "unit_price", "invoice"})
_supplier_reputation = _plain(SupplierReputation, {"reputation_id", "supplier_id", "component_id", "agent_id", "offered", "purchased", "reputation_ppm"})
_customer_rfq = _plain(CustomerRfq, {"rfq_id", "product_id", "quantity", "due_day", "unit_reserve", "daily_penalty"})
_customer_order = _plain(CustomerOrder, {"order_id", "rfq_id", "agent_id", "product_id", "quantity", "due_day", "unit_price", "daily_penalty", "penalty_count", "status", "payment_status", "delivery_day"})
_customer_settlement = _plain(CustomerSettlement, {"settlement_id", "order_id", "delivered", "payment", "penalty", "penalty_count", "cancelled"})
_default = _plain(DefaultRecord, {"day", "cause"})
_price = _plain(PriceRange, {"product_id", "minimum", "maximum"})
_product_market = _plain(ProductMarketRow, {"product_id", "rfq_count", "rfq_quantity", "order_count", "order_quantity", "minimum_price", "maximum_price", "mean_price"})
_component_market = _plain(ComponentMarketRow, {"component_id", "ordered_quantity", "delivered_quantity", "mean_unit_price"})
_capacity_market = _plain(CapacityMarketRow, {"supplier_id", "component_id", "mean_capacity_nano"})


def _action_disposition(value: Any) -> ActionDisposition:
    value = _closed(value, {"day", "action_digest", "cause"}, {"revision"}, "action-disposition")
    return ActionDisposition(value["day"], value["action_digest"], value["cause"], value.get("revision"))


def _daily_report(value: Any) -> DailyPriceReport:
    value = _closed(value, {"day", "products"}, name="daily-price-report")
    return DailyPriceReport(value["day"], _tuple(value["products"], _price, "products"))


def _market_report(value: Any) -> MarketReport:
    value = _closed(value, {"start_day", "end_day", "products", "components", "supplier_lines"}, name="market-report")
    return MarketReport(value["start_day"], value["end_day"], _tuple(value["products"], _product_market, "products"), _tuple(value["components"], _component_market, "components"), _tuple(value["supplier_lines"], _capacity_market, "supplier_lines"))


def _catalog(value: Any) -> Catalog:
    value = _closed(value, {"components", "products", "suppliers"}, name="catalog")
    def component(item: Any) -> Component:
        item = _closed(item, {"id", "base_microcredits", "suppliers"}, name="component")
        return Component(item["id"], item["base_microcredits"], tuple(item["suppliers"]))

    def product(item: Any) -> Product:
        item = _closed(item, {"id", "components", "cycles", "segment"}, name="product")
        return Product(item["id"], tuple(item["components"]), item["cycles"], item["segment"])

    def supplier(item: Any) -> Supplier:
        item = _closed(item, {"id", "components"}, name="supplier")
        return Supplier(item["id"], tuple(item["components"]))

    components = _tuple(value["components"], component, "components")
    products = _tuple(value["products"], product, "products")
    suppliers = _tuple(value["suppliers"], supplier, "suppliers")
    return Catalog(components, products, suppliers)


def parse_payload(value: Any, *, agent_id: str | None = None) -> Payload:
    if not isinstance(value, dict) or not isinstance(value.get("type"), str):
        raise ContractError("payload must be a typed map")
    kind = value["type"]
    if kind == "day-observation":
        keys = {"type", "observation_id", "day", "state_commitment", "event_cursor", "cash_microcredits", "ledger_since_previous", "component_inventory", "pc_inventory", "pending_inventory", "production_results", "delivery_results", "supplier_rfqs", "supplier_offers", "supplier_orders", "supplier_deliveries", "supplier_reputations", "customer_rfqs", "new_customer_orders", "active_customer_orders", "customer_settlements", "previous_action", "daily_price_report", "market_report", "default_history"}
        value = _closed(value, keys, name="day-observation")
        if agent_id is None:
            raise ContractError("observation requires its envelope recipient")
        return DayObservation(
            agent_id, value["observation_id"], value["day"], value["state_commitment"], value["cash_microcredits"], value["event_cursor"],
            _tuple(value["ledger_since_previous"], _ledger, "ledger_since_previous"), _tuple(value["component_inventory"], _inventory, "component_inventory"),
            _tuple(value["pc_inventory"], _inventory, "pc_inventory"), _tuple(value["pending_inventory"], _inventory, "pending_inventory"),
            _tuple(value["production_results"], _schedule_result, "production_results"), _tuple(value["delivery_results"], _schedule_result, "delivery_results"),
            _tuple(value["supplier_rfqs"], _supplier_rfq, "supplier_rfqs"), _tuple(value["supplier_offers"], _supplier_offer, "supplier_offers"),
            _tuple(value["supplier_orders"], _supplier_order, "supplier_orders"), _tuple(value["supplier_deliveries"], _supplier_delivery, "supplier_deliveries"),
            _tuple(value["supplier_reputations"], _supplier_reputation, "supplier_reputations"), _tuple(value["customer_rfqs"], _customer_rfq, "customer_rfqs"),
            _tuple(value["new_customer_orders"], _customer_order, "new_customer_orders"), _tuple(value["active_customer_orders"], _customer_order, "active_customer_orders"),
            _tuple(value["customer_settlements"], _customer_settlement, "customer_settlements"),
            _action_disposition(value["previous_action"]) if value["previous_action"] is not None else None,
            _daily_report(value["daily_price_report"]), _market_report(value["market_report"]) if value["market_report"] is not None else None,
            _tuple(value["default_history"], _default, "default_history"),
        )
    if kind == "day-action-set":
        value = _closed(value, {"type", "agent_id", "observation_id", "day", "revision", "customer_offers", "supplier_rfqs", "supplier_orders", "production_schedule", "delivery_schedule"}, name=kind)
        return DayActionSet(value["agent_id"], value["observation_id"], value["day"], value["revision"],
            _tuple(value["customer_offers"], _plain(CustomerOfferCommand, {"command_id", "rfq_id", "unit_price"}), "customer_offers"),
            _tuple(value["supplier_rfqs"], _plain(SupplierRfqCommand, {"command_id", "rfq_id", "supplier_id", "component_id", "quantity", "due_day", "max_unit_price"}), "supplier_rfqs"),
            _tuple(value["supplier_orders"], _plain(SupplierOrderCommand, {"command_id", "offer_id"}), "supplier_orders"),
            _tuple(value["production_schedule"], _plain(ProductionLine, {"line_id", "priority", "product_id", "quantity"}), "production_schedule"),
            _tuple(value["delivery_schedule"], _plain(DeliveryLine, {"line_id", "priority", "customer_order_id", "quantity"}), "delivery_schedule"))
    if kind == "game-bootstrap":
        value = _closed(value, {"type", "game_definition_id", "schema", "profile", "modules", "agent_id", "agent_slot", "roster", "semantic_configuration", "catalog", "latest_committed_day"}, name=kind)
        return GameBootstrap(value["game_definition_id"], _artifact(value["schema"]), _artifact(value["profile"]), _tuple(value["modules"], _artifact, "modules"), value["agent_id"], value["agent_slot"], tuple(value["roster"]), _freeze(value["semantic_configuration"]), _catalog(value["catalog"]), value["latest_committed_day"])
    if kind == "action-receipt":
        value = _closed(value, {"type", "day", "revision", "action_digest", "receipt_ordinal", "candidate_valid", "errors"}, name=kind)
        return ActionReceipt(value["day"], value["revision"], value["action_digest"], value["receipt_ordinal"], value["candidate_valid"], _tuple(value["errors"], _rejection, "errors"))
    if kind == "action-committed":
        value = _closed(value, {"type", "day", "action_digest", "cause", "command_results"}, name=kind)
        def command(item: Any) -> CommandResult:
            item = _closed(item, {"command_id", "accepted"}, {"rejection"}, "command-result")
            return CommandResult(item["command_id"], item["accepted"], _rejection(item["rejection"]) if "rejection" in item else None)
        return ActionCommitted(value["day"], value["action_digest"], value["cause"], _tuple(value["command_results"], command, "command_results"))
    if kind == "game-finished":
        value = _closed(value, {"type", "status", "scores", "final_state_commitment"}, {"abort_reason"}, name=kind)
        scores = None if value["scores"] is None else _tuple(value["scores"], _plain(Score, {"agent_id", "cash_microcredits"}), "scores")
        return GameFinished(value["status"], scores, value["final_state_commitment"], value.get("abort_reason"))
    raise ContractError(f"unknown payload type: {kind}")


def logical_value(record: Any) -> Any:
    """Project an immutable record into its closed canonical logical value."""

    if is_dataclass(record):
        result = {}
        for descriptor in record.__dataclass_fields__.values():
            if isinstance(record, DayObservation) and descriptor.name == "agent_id":
                continue
            value = getattr(record, descriptor.name)
            if value is None and descriptor.name in {"game_day", "causation_id", "final_submission", "abort_reason", "revision", "rejection"}:
                continue
            result[descriptor.name] = logical_value(value)
        return result
    if isinstance(record, tuple):
        return [logical_value(item) for item in record]
    if isinstance(record, Mapping):
        return {key: logical_value(item) for key, item in record.items()}
    return record
