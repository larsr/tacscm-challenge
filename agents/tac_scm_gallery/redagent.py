"""Bounded RedAgent-inspired internal markets over public observations only."""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from typing import Protocol

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    CustomerOrder,
    DayActionSet,
    DayObservation,
    DeliveryLine,
    ProductionLine,
    SupplierOrderCommand,
    SupplierOffer,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from .contracts import DecisionExplanation, FeatureVector, KernelDecision


class _InventoryRecord(Protocol):
    item_id: str
    quantity: int


@dataclass(frozen=True, slots=True)
class MarketOrder:
    order_id: str
    quantity: int
    unit_value: int

    def __post_init__(self) -> None:
        if self.quantity < 0 or self.unit_value < 0:
            raise ValueError("market orders require non-negative quantity and value")


@dataclass(frozen=True, slots=True)
class MarketClearing:
    unit_value: int
    quantity: int
    allocations: tuple[tuple[str, int], ...]


@dataclass(slots=True)
class _BookOrder:
    order_id: str
    quantity: int
    unit_value: int


@dataclass(frozen=True, slots=True)
class ProductIntent:
    intent_id: str
    product_id: str
    quantity: int
    due_day: int
    unit_value: int
    committed: bool


@dataclass(frozen=True, slots=True)
class InternalAllocation:
    component_values: tuple[tuple[str, int], ...]
    product_values: tuple[tuple[str, int], ...]
    cycle_value: int
    feasible_units: tuple[tuple[str, int], ...]
    rounds: int


def clear_uniform_market(
    asks: Iterable[MarketOrder], bids: Iterable[MarketOrder]
) -> MarketClearing:
    """Clear a deterministic divisible double auction at one uniform value."""

    supply = [_BookOrder(order.order_id, order.quantity, order.unit_value) for order in asks]
    demand = [_BookOrder(order.order_id, order.quantity, order.unit_value) for order in bids]
    supply.sort(key=lambda row: (row.unit_value, row.order_id))
    demand.sort(key=lambda row: (-row.unit_value, row.order_id))
    allocations: dict[str, int] = {}
    supply_index = demand_index = 0
    quantity = 0
    marginal_ask = marginal_bid = 0
    while supply_index < len(supply) and demand_index < len(demand):
        ask = supply[supply_index]
        bid = demand[demand_index]
        if bid.unit_value < ask.unit_value:
            break
        traded = min(ask.quantity, bid.quantity)
        if traded <= 0:
            if ask.quantity == 0:
                supply_index += 1
            if bid.quantity == 0:
                demand_index += 1
            continue
        ask.quantity -= traded
        bid.quantity -= traded
        quantity += traded
        marginal_ask, marginal_bid = ask.unit_value, bid.unit_value
        allocations[bid.order_id] = allocations.get(bid.order_id, 0) + traded
        if ask.quantity == 0:
            supply_index += 1
        if bid.quantity == 0:
            demand_index += 1
    value = (marginal_ask + marginal_bid) // 2 if quantity else 0
    return MarketClearing(value, quantity, tuple(sorted(allocations.items())))


def _inventory(records: Iterable[_InventoryRecord]) -> dict[str, int]:
    result: dict[str, int] = {}
    for record in records:
        result[record.item_id] = result.get(record.item_id, 0) + record.quantity
    return result


def _nominal_product(product_id: str) -> int:
    return sum(BASE_MICROCREDITS[item] for item in PRODUCTS[product_id][0])


def _open_orders(observation: DayObservation) -> tuple[CustomerOrder, ...]:
    return tuple(
        sorted(
            (order for order in observation.active_customer_orders if order.status == "open"),
            key=lambda order: (order.due_day, order.order_id),
        )
    )


def _supply_position(observation: DayObservation) -> dict[str, int]:
    supply = _inventory(observation.component_inventory)
    for item in observation.pending_inventory:
        if item.item_id in SUPPLIERS:
            supply[item.item_id] = supply.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        supply[order.component_id] = supply.get(order.component_id, 0) + max(
            0, order.quantity - order.delivered_quantity
        )
    return supply


def _observed_product_value(observation: DayObservation, product_id: str) -> int:
    values: list[int] = []
    if observation.market_report is not None:
        for row in observation.market_report.products:
            if row.product_id == product_id and row.mean_price is not None:
                values.append(row.mean_price)
    if observation.daily_price_report is not None:
        for row in observation.daily_price_report.products:
            if row.product_id == product_id:
                if row.minimum is not None:
                    values.append(row.minimum)
                if row.maximum is not None:
                    values.append(row.maximum)
    return min(values) if values else _nominal_product(product_id) * 6 // 5


def build_intents(observation: DayObservation) -> tuple[ProductIntent, ...]:
    intents: list[ProductIntent] = []
    for order in _open_orders(observation):
        intents.append(
            ProductIntent(
                f"order-{order.order_id}", order.product_id, order.quantity,
                order.due_day, order.unit_price + order.daily_penalty, True,
            )
        )
    if observation.day <= 213:
        for rfq in observation.customer_rfqs:
            if rfq.due_day - observation.day >= 4:
                intents.append(
                    ProductIntent(
                        f"rfq-{rfq.rfq_id}", rfq.product_id, rfq.quantity,
                        rfq.due_day, min(rfq.unit_reserve, _observed_product_value(observation, rfq.product_id)), False,
                    )
                )
    return tuple(sorted(intents, key=lambda item: (not item.committed, item.due_day, item.intent_id)))


def allocate_internal_market(
    observation: DayObservation,
    intents: tuple[ProductIntent, ...],
    *,
    enabled: bool,
    rounds: int = 3,
) -> InternalAllocation:
    """Discover resource values, then reconcile allocations to complete bundles."""

    if rounds < 1 or rounds > 8:
        raise ValueError("market rounds must be in 1..8")
    supply = _supply_position(observation)
    component_values = dict(BASE_MICROCREDITS)
    cycle_value = 20_000_000
    feasible: dict[str, int] = {intent.intent_id: 0 for intent in intents}
    iterations = rounds if enabled else 1
    for _ in range(iterations):
        allocations: dict[str, dict[str, int]] = {}
        next_values: dict[str, int] = {}
        for component in sorted(SUPPLIERS):
            bids = []
            for intent in intents:
                if component not in PRODUCTS[intent.product_id][0]:
                    continue
                bom_residual = max(
                    0,
                    intent.unit_value
                    - PRODUCTS[intent.product_id][1] * cycle_value
                    - sum(
                        component_values[item]
                        for item in PRODUCTS[intent.product_id][0]
                        if item != component
                    ),
                )
                bids.append(MarketOrder(intent.intent_id, intent.quantity, bom_residual))
            clearing = clear_uniform_market(
                (MarketOrder(f"stock-{component}", supply.get(component, 0), BASE_MICROCREDITS[component]),),
                bids,
            )
            allocations[component] = dict(clearing.allocations)
            next_values[component] = (
                clearing.unit_value if enabled and clearing.quantity else BASE_MICROCREDITS[component]
            )
        cycle_bids = []
        for intent in intents:
            components, cycles = PRODUCTS[intent.product_id]
            residual = max(0, intent.unit_value - sum(component_values[item] for item in components))
            cycle_bids.append(MarketOrder(intent.intent_id, intent.quantity * cycles, residual // cycles))
        cycle_clearing = clear_uniform_market(
            (MarketOrder("factory-cycles", 2_000, 20_000_000),), cycle_bids
        )
        cycle_allocations = dict(cycle_clearing.allocations)
        for intent in intents:
            components, cycles = PRODUCTS[intent.product_id]
            feasible[intent.intent_id] = min(
                [intent.quantity, cycle_allocations.get(intent.intent_id, 0) // cycles]
                + [allocations[item].get(intent.intent_id, 0) for item in components]
            )
        component_values = next_values if enabled else dict(BASE_MICROCREDITS)
        cycle_value = (
            cycle_clearing.unit_value
            if enabled and cycle_clearing.quantity
            else 20_000_000
        )
    product_values = {
        product_id: sum(component_values[item] for item in components) + cycles * cycle_value
        for product_id, (components, cycles) in PRODUCTS.items()
    }
    return InternalAllocation(
        tuple(sorted(component_values.items())),
        tuple(sorted(product_values.items())),
        cycle_value,
        tuple(sorted(feasible.items())),
        iterations,
    )


class RedAgentMarketKernel:
    """Publication-inspired, 2026-bounded internal market allocator."""

    MAX_OPEN_UNITS = 32
    MAX_POSITION_UNITS = 256
    DEBT_FLOOR = -100_000_000_000
    MIN_MARGIN_PPB = 180_000_000
    SUPPLIER_VALUE_CAP_PPB = 750_000_000
    strategy_id = "redagent-market-v1"

    def __init__(self, *, market_enabled: bool = True) -> None:
        self.market_enabled = market_enabled
        if not market_enabled:
            self.strategy_id = "redagent-no-market-v1"

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: object,
    ) -> KernelDecision:
        del features, history
        if observation.day == 219:
            return KernelDecision(
                empty_action(observation),
                DecisionExplanation(self.strategy_id, "terminal", "No commitments on the final day.", ("endgame",)),
            )
        orders = _open_orders(observation)
        intents = build_intents(observation)
        market = allocate_internal_market(observation, intents, enabled=self.market_enabled)
        component_values = dict(market.component_values)
        product_values = dict(market.product_values)
        feasible = dict(market.feasible_units)
        deliveries = self._deliveries(observation, orders)
        production = self._production(observation, orders)
        supplier_orders = self._supplier_orders(observation, component_values)
        customer_offers, supplier_rfqs = self._new_business(
            observation, orders, intents, feasible, component_values, product_values
        )
        action = DayActionSet(
            observation.agent_id, observation.observation_id, observation.day, 1,
            customer_offers, supplier_rfqs, supplier_orders, production, deliveries,
        )
        open_units = sum(order.quantity for order in orders)
        constrained = sum(
            1 for intent in intents if feasible.get(intent.intent_id, 0) < intent.quantity
        )
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "internal-market" if self.market_enabled else "nominal-ablation",
                "Clear bounded resource markets and commit only complete profitable bundles.",
                ("uniform-double-auction", "complement-reconciliation", "cash-and-endgame-bounds"),
                (
                    ("market_rounds", market.rounds),
                    ("cycle_value_microcredits", market.cycle_value),
                    ("constrained_intents", constrained),
                    ("open_order_units", open_units),
                    ("customer_offers", len(customer_offers)),
                    ("supplier_orders", len(supplier_orders)),
                ),
            ),
        )

    def _new_business(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        intents: tuple[ProductIntent, ...],
        feasible: dict[str, int],
        component_values: dict[str, int],
        product_values: dict[str, int],
    ) -> tuple[tuple[CustomerOfferCommand, ...], tuple[SupplierRfqCommand, ...]]:
        open_units = sum(order.quantity for order in orders)
        position = sum(
            item.quantity
            for item in observation.component_inventory + observation.pc_inventory + observation.pending_inventory
        ) + sum(max(0, order.quantity - order.delivered_quantity) for order in observation.supplier_orders)
        if (
            observation.day > 200
            or observation.cash_microcredits <= self.DEBT_FLOOR
            or open_units >= self.MAX_OPEN_UNITS
            or position >= self.MAX_POSITION_UNITS
        ):
            return (), ()
        candidates = []
        by_id = {intent.intent_id: intent for intent in intents}
        for rfq in observation.customer_rfqs:
            intent = by_id.get(f"rfq-{rfq.rfq_id}")
            if intent is None or open_units + rfq.quantity > self.MAX_OPEN_UNITS:
                continue
            internal_cost = (
                3 * product_values[rfq.product_id]
                + _nominal_product(rfq.product_id)
            ) // 4
            minimum = internal_cost * (1_000_000_000 + self.MIN_MARGIN_PPB) // 1_000_000_000
            market_anchor = _observed_product_value(observation, rfq.product_id) * 98 // 100
            price = min(rfq.unit_reserve, max(minimum, market_anchor))
            if price < minimum:
                continue
            scarcity = rfq.quantity - feasible.get(intent.intent_id, 0)
            surplus = (
                (price - internal_cost) * rfq.quantity
                - scarcity * internal_cost // 20
            )
            if surplus > 0:
                candidates.append((-surplus, rfq.due_day, rfq.rfq_id, rfq, price))
        selected = sorted(candidates)[:1]
        offers = tuple(
            CustomerOfferCommand(f"co-{observation.day}-{index}", row[3].rfq_id, row[4])
            for index, row in enumerate(selected)
        )
        if observation.day > 199:
            return offers, ()
        demand: dict[str, int] = {component: 0 for component in SUPPLIERS}
        pc_stock = _inventory(observation.pc_inventory)
        for order in orders:
            required = max(0, order.quantity - pc_stock.get(order.product_id, 0))
            pc_stock[order.product_id] = max(0, pc_stock.get(order.product_id, 0) - order.quantity)
            for component in PRODUCTS[order.product_id][0]:
                demand[component] += required
        for row in selected:
            rfq = row[3]
            for component in PRODUCTS[rfq.product_id][0]:
                demand[component] += rfq.quantity
        supply = _supply_position(observation)
        requests: list[SupplierRfqCommand] = []
        due_by_component: dict[str, list[int]] = {
            component: [] for component in SUPPLIERS
        }
        for order in orders:
            for component in PRODUCTS[order.product_id][0]:
                due_by_component[component].append(order.due_day)
        for row in selected:
            for component in PRODUCTS[row[3].product_id][0]:
                due_by_component[component].append(row[3].due_day)
        for component in sorted(SUPPLIERS):
            shortage = max(0, demand[component] - supply.get(component, 0))
            if shortage == 0 or position + sum(item.quantity for item in requests) + shortage > self.MAX_POSITION_UNITS:
                continue
            requests.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(requests)}",
                    f"ra{observation.day:03d}-{len(requests)}",
                    min(SUPPLIERS[component]), component, shortage,
                    max(observation.day, min(due_by_component[component]) - 2),
                    component_values[component],
                )
            )
            if len(requests) == 8:
                break
        return offers, tuple(requests)

    def _supplier_orders(
        self, observation: DayObservation, component_values: dict[str, int]
    ) -> tuple[SupplierOrderCommand, ...]:
        if observation.day > 212:
            return ()
        orders = _open_orders(observation)
        pc_stock = _inventory(observation.pc_inventory)
        demand = {component: 0 for component in SUPPLIERS}
        for order in orders:
            required = max(0, order.quantity - pc_stock.get(order.product_id, 0))
            pc_stock[order.product_id] = max(
                0, pc_stock.get(order.product_id, 0) - order.quantity
            )
            for component in PRODUCTS[order.product_id][0]:
                demand[component] += required
        supply = _supply_position(observation)
        shortages = {
            component: max(0, quantity - supply.get(component, 0))
            for component, quantity in demand.items()
        }
        if not any(shortages.values()):
            return ()
        groups: dict[str, list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            if offer.kind in {"none", "probe"} or offer.quantity <= 0:
                continue
            cap = component_values[offer.component_id] * self.SUPPLIER_VALUE_CAP_PPB // 1_000_000_000
            if offer.unit_price <= cap and offer.quantity >= shortages[offer.component_id] > 0:
                groups.setdefault(offer.component_id, []).append(offer)
        required_components = {component for component, quantity in shortages.items() if quantity > 0}
        if set(groups) != required_components:
            return ()
        outstanding = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        selected: list[SupplierOrderCommand] = []
        spend = 0
        for component in sorted(groups):
            offer = min(
                groups[component],
                key=lambda item: (item.unit_price, item.due_day, item.offer_id),
            )
            cost = offer.quantity * offer.unit_price
            if observation.cash_microcredits - outstanding - spend - cost < self.DEBT_FLOOR:
                return ()
            selected.append(SupplierOrderCommand(f"so-{observation.day}-{len(selected)}", offer.offer_id))
            spend += cost
        return tuple(selected)

    @staticmethod
    def _production(
        observation: DayObservation, orders: tuple[CustomerOrder, ...]
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles = 2_000
        lines: list[ProductionLine] = []
        for order in orders:
            required = max(0, order.quantity - pcs.get(order.product_id, 0))
            pcs[order.product_id] = max(0, pcs.get(order.product_id, 0) - order.quantity)
            bom, unit_cycles = PRODUCTS[order.product_id]
            feasible = min(
                [required, cycles // unit_cycles]
                + [components.get(item, 0) for item in bom]
            )
            if feasible != required or required <= 0:
                continue
            for item in bom:
                components[item] -= feasible
            cycles -= feasible * unit_cycles
            lines.append(ProductionLine(f"pl-{observation.day}-{len(lines)}", len(lines), order.product_id, feasible))
        return tuple(lines)

    @staticmethod
    def _deliveries(
        observation: DayObservation, orders: tuple[CustomerOrder, ...]
    ) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        lines: list[DeliveryLine] = []
        for order in orders:
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            lines.append(DeliveryLine(f"dl-{observation.day}-{len(lines)}", len(lines), order.order_id, order.quantity))
        return tuple(lines)
