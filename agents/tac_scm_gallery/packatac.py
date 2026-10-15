"""Bounded PackaTAC-inspired reservation and risk-control policy."""

from __future__ import annotations

from dataclasses import dataclass

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    CustomerOrder,
    CustomerRfq,
    DayActionSet,
    DayObservation,
    DeliveryLine,
    InventoryItem,
    ProductionLine,
    SupplierOffer,
    SupplierOrderCommand,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from .contracts import (
    DecisionExplanation,
    FeatureVector,
    KernelDecision,
    PublicHistoryProtocol,
)


@dataclass(frozen=True, slots=True)
class RiskBudget:
    buffer_units: int
    debt_floor: int
    maximum_position: int
    maximum_open_customer_units: int
    maximum_backorder_units: int
    minimum_margin_ppm: int
    supplier_cap_ppm: int
    count_pending_for_bids: bool
    cycle_reservation_ppm: int


STRICT_BUDGET = RiskBudget(
    2,
    -80_000_000_000,
    48,
    8,
    4,
    180_000,
    750_000,
    False,
    1_000_000,
)
RELAXED_BUDGET = RiskBudget(
    8,
    -120_000_000_000,
    140,
    32,
    20,
    80_000,
    900_000,
    True,
    700_000,
)


@dataclass(frozen=True, slots=True)
class ReservationLedger:
    available_components: tuple[tuple[str, int], ...]
    available_pcs: tuple[tuple[str, int], ...]
    cycle_headroom: tuple[tuple[int, int], ...]
    protected_component_units: int
    open_customer_units: int
    backorder_units: int
    cash_headroom: int
    relaxed: bool


def _inventory(records: tuple[InventoryItem, ...]) -> dict[str, int]:
    result: dict[str, int] = {}
    for record in records:
        result[record.item_id] = result.get(record.item_id, 0) + record.quantity
    return result


def _open_orders(observation: DayObservation) -> tuple[CustomerOrder, ...]:
    return tuple(
        sorted(
            (
                order
                for order in observation.active_customer_orders
                if order.status == "open"
            ),
            key=lambda order: (order.due_day, order.order_id),
        )
    )


def _nominal_product(product_id: str) -> int:
    return sum(BASE_MICROCREDITS[item] for item in PRODUCTS[product_id][0])


def _buffer_rfq_id(component_id: str) -> str:
    return f"pk-{component_id}"


def _parse_buffer_rfq_id(value: str) -> str | None:
    if not value.startswith("pk-"):
        return None
    component = value[3:]
    return component if component in SUPPLIERS else None


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


def build_reservation_ledger(
    observation: DayObservation, *, relaxed: bool
) -> ReservationLedger:
    """Reserve current public inventory and dated cycles for all open orders."""

    budget = RELAXED_BUDGET if relaxed else STRICT_BUDGET
    components = _inventory(observation.component_inventory)
    pcs = _inventory(observation.pc_inventory)
    if budget.count_pending_for_bids:
        for item in observation.pending_inventory:
            if item.item_id in SUPPLIERS:
                components[item.item_id] = components.get(item.item_id, 0) + item.quantity
        for supplier_order in observation.supplier_orders:
            if supplier_order.due_day <= observation.day + 8:
                components[supplier_order.component_id] = components.get(
                    supplier_order.component_id, 0
                ) + max(
                    0,
                    supplier_order.quantity - supplier_order.delivered_quantity,
                )
    cycles = {
        day: 2_000 * budget.cycle_reservation_ppm // 1_000_000
        for day in range(observation.day, min(219, observation.day + 10) + 1)
    }
    protected = 0
    orders = _open_orders(observation)
    for order in orders:
        covered = min(pcs.get(order.product_id, 0), order.quantity)
        pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
        required = order.quantity - covered
        bom, product_cycles = PRODUCTS[order.product_id]
        for component in bom:
            reserved = min(components.get(component, 0), required)
            components[component] = components.get(component, 0) - reserved
            protected += reserved
        production_day = max(observation.day, min(219, order.due_day - 1))
        if production_day in cycles:
            cycles[production_day] -= required * product_cycles
    backorder = sum(
        max(0, order.quantity - order.delivered_quantity)
        for order in observation.supplier_orders
    )
    outstanding_cost = sum(
        max(0, order.quantity - order.delivered_quantity) * order.unit_price
        for order in observation.supplier_orders
    )
    return ReservationLedger(
        tuple(sorted(components.items())),
        tuple(sorted(pcs.items())),
        tuple(sorted(cycles.items())),
        protected,
        sum(order.quantity for order in orders),
        backorder,
        observation.cash_microcredits - outstanding_cost - budget.debt_floor,
        relaxed,
    )


def _observed_price(observation: DayObservation, product_id: str) -> int | None:
    if observation.daily_price_report is not None:
        for price_range in observation.daily_price_report.products:
            if price_range.product_id == product_id:
                values = [
                    value
                    for value in (price_range.minimum, price_range.maximum)
                    if value is not None
                ]
                if values:
                    return min(values) + (max(values) - min(values)) * 3 // 4
    if observation.market_report is not None:
        for market_row in observation.market_report.products:
            if market_row.product_id == product_id:
                return market_row.mean_price
    return None


def _liquidation_price(observation: DayObservation, rfq: CustomerRfq) -> int:
    recent = _observed_price(observation, rfq.product_id)
    anchor = rfq.unit_reserve if recent is None else recent * 19 // 20
    return min(rfq.unit_reserve, max(1, anchor))


class PackaTacRiskKernel:
    """Publication-inspired conservative reservation and follower strategy."""

    strategy_id = "packatac-strict-v1"

    def __init__(self, *, relaxed: bool = False) -> None:
        self.relaxed = relaxed
        self.budget = RELAXED_BUDGET if relaxed else STRICT_BUDGET
        if relaxed:
            self.strategy_id = "packatac-relaxed-v1"

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: PublicHistoryProtocol,
    ) -> KernelDecision:
        del features, history
        if observation.day == 219:
            return KernelDecision(
                empty_action(observation),
                DecisionExplanation(
                    self.strategy_id,
                    "terminal",
                    "No commitments on the final day.",
                    ("endgame",),
                ),
            )
        ledger = build_reservation_ledger(observation, relaxed=self.relaxed)
        orders = _open_orders(observation)
        deliveries = self._deliveries(observation, orders)
        production = self._production(observation, orders)
        supplier_orders = self._supplier_orders(observation)
        offers = self._customer_offers(observation, ledger)
        supplier_rfqs = self._buffer_requests(observation)
        liquidation = observation.day >= 195 or self._position(observation) > 96
        rejected = sum(
            1
            for rfq in observation.customer_rfqs
            if all(offer.rfq_id != rfq.rfq_id for offer in offers)
        )
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            offers,
            supplier_rfqs,
            supplier_orders,
            production,
            deliveries,
        )
        component_headroom = sum(quantity for _, quantity in ledger.available_components)
        cycle_headroom = min((value for _, value in ledger.cycle_headroom), default=0)
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "relaxed-envelope" if self.relaxed else "strict-envelope",
                "Reserve current complete-BOM inventory and dated capacity before bidding.",
                (
                    "all-bids-can-win",
                    "current-stock-cover",
                    "bounded-backorders",
                ),
                (
                    ("protected_component_units", ledger.protected_component_units),
                    ("component_headroom_units", component_headroom),
                    ("cycle_headroom", cycle_headroom),
                    ("cash_headroom_microcredits", ledger.cash_headroom),
                    ("backorder_units", ledger.backorder_units),
                    ("rejected_opportunities", rejected),
                    ("liquidation_mode", int(liquidation)),
                    ("customer_offers", len(offers)),
                    (
                        "active_customer_value_microcredits",
                        sum(order.quantity * order.unit_price for order in orders),
                    ),
                    (
                        "customer_payment_microcredits",
                        sum(
                            settlement.payment
                            for settlement in observation.customer_settlements
                        ),
                    ),
                ),
            ),
        )

    def _customer_offers(
        self,
        observation: DayObservation,
        ledger: ReservationLedger,
    ) -> tuple[CustomerOfferCommand, ...]:
        if (
            observation.day > 205
            or ledger.open_customer_units >= self.budget.maximum_open_customer_units
        ):
            return ()
        components = dict(ledger.available_components)
        pcs = dict(ledger.available_pcs)
        cycles = dict(ledger.cycle_headroom)
        liquidation = observation.day >= 195 or self._position(observation) > 96
        candidates: list[tuple[int, int, str, CustomerRfq, int]] = []
        for rfq in observation.customer_rfqs:
            lead = rfq.due_day - observation.day
            if (
                not 4 <= lead <= 10
                or ledger.open_customer_units + rfq.quantity
                > self.budget.maximum_open_customer_units
            ):
                continue
            covered_pc = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            required = rfq.quantity - covered_pc
            bom, product_cycles = PRODUCTS[rfq.product_id]
            production_day = max(observation.day, rfq.due_day - 1)
            if any(components.get(component, 0) < required for component in bom):
                continue
            if cycles.get(production_day, 0) < required * product_cycles:
                continue
            nominal = _nominal_product(rfq.product_id)
            hard_floor = (
                nominal * (1_000_000 + self.budget.minimum_margin_ppm) // 1_000_000
            )
            observed = _observed_price(observation, rfq.product_id)
            follower = rfq.unit_reserve if observed is None else observed
            price = (
                _liquidation_price(observation, rfq)
                if liquidation
                else min(rfq.unit_reserve, max(hard_floor, follower))
            )
            if not liquidation and price < hard_floor:
                continue
            inventory_priority = sum(components.get(component, 0) for component in bom)
            surplus = (price - nominal) * rfq.quantity
            candidates.append((-inventory_priority, -surplus, rfq.rfq_id, rfq, price))
        offers: list[CustomerOfferCommand] = []
        for _, _, _, rfq, price in sorted(candidates):
            covered_pc = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - covered_pc
            required = rfq.quantity - covered_pc
            bom, product_cycles = PRODUCTS[rfq.product_id]
            production_day = max(observation.day, rfq.due_day - 1)
            if any(components.get(component, 0) < required for component in bom):
                continue
            if cycles.get(production_day, 0) < required * product_cycles:
                continue
            for component in bom:
                components[component] -= required
            cycles[production_day] -= required * product_cycles
            offers.append(
                CustomerOfferCommand(
                    f"co-{observation.day}-{len(offers)}", rfq.rfq_id, price
                )
            )
            if len(offers) == 4:
                break
        return tuple(offers)

    def _buffer_requests(
        self, observation: DayObservation
    ) -> tuple[SupplierRfqCommand, ...]:
        if (
            observation.day > 190
            or observation.cash_microcredits <= self.budget.debt_floor
            or (
                not self.relaxed
                and observation.day > 0
                and observation.cash_microcredits < 0
            )
        ):
            return ()
        position = self._position(observation)
        supply = _supply_position(observation)
        requested_by_component: dict[str, int] = {}
        for request in observation.supplier_rfqs:
            component = _parse_buffer_rfq_id(request.client_rfq_id)
            if component is not None:
                requested_by_component[component] = (
                    requested_by_component.get(component, 0) + request.quantity
                )
        open_by_component = {
            component: sum(
                max(0, order.quantity - order.delivered_quantity)
                for order in observation.supplier_orders
                if order.component_id == component
            )
            for component in SUPPLIERS
        }
        shortages = sorted(
            (
                self.budget.buffer_units
                - supply.get(component, 0)
                - requested_by_component.get(component, 0),
                component,
            )
            for component in SUPPLIERS
            if supply.get(component, 0) + requested_by_component.get(component, 0)
            < self.budget.buffer_units
            and open_by_component[component] < self.budget.maximum_backorder_units
        )
        requests: list[SupplierRfqCommand] = []
        for shortage, component in sorted(shortages, key=lambda row: (-row[0], row[1])):
            quantity = min(shortage, self.budget.maximum_backorder_units)
            if position + sum(item.quantity for item in requests) + quantity > self.budget.maximum_position:
                continue
            requests.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(requests)}",
                    _buffer_rfq_id(component),
                    min(SUPPLIERS[component]),
                    component,
                    quantity,
                    observation.day + 4,
                    BASE_MICROCREDITS[component]
                    * self.budget.supplier_cap_ppm
                    // 1_000_000,
                )
            )
            if len(requests) == 8:
                break
        return tuple(requests)

    def _supplier_orders(
        self, observation: DayObservation
    ) -> tuple[SupplierOrderCommand, ...]:
        if observation.day > 194:
            return ()
        request_by_id = {
            request.rfq_id: request for request in observation.supplier_rfqs
        }
        supply = _supply_position(observation)
        groups: dict[str, list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            request = request_by_id.get(offer.rfq_id)
            component = (
                None
                if request is None
                else _parse_buffer_rfq_id(request.client_rfq_id)
            )
            if (
                component is None
                or offer.component_id != component
                or offer.kind in {"none", "probe"}
                or offer.quantity <= 0
                or offer.unit_price
                > BASE_MICROCREDITS[component]
                * self.budget.supplier_cap_ppm
                // 1_000_000
            ):
                continue
            groups.setdefault(component, []).append(offer)
        outstanding = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        position = self._position(observation)
        selected: list[SupplierOrderCommand] = []
        spend = 0
        selected_units = 0
        for component in sorted(groups):
            shortage = max(0, self.budget.buffer_units - supply.get(component, 0))
            candidates = [
                offer
                for offer in groups[component]
                if shortage <= offer.quantity <= shortage + 2
            ]
            if shortage == 0 or not candidates:
                continue
            offer = min(
                candidates,
                key=lambda item: (
                    item.quantity - shortage,
                    item.unit_price,
                    item.due_day,
                    item.offer_id,
                ),
            )
            cost = offer.quantity * offer.unit_price
            if (
                observation.cash_microcredits
                - outstanding
                - spend
                - cost
                < self.budget.debt_floor
                or position + selected_units + offer.quantity
                > self.budget.maximum_position
            ):
                continue
            selected.append(
                SupplierOrderCommand(
                    f"so-{observation.day}-{len(selected)}", offer.offer_id
                )
            )
            spend += cost
            selected_units += offer.quantity
        return tuple(selected)

    @staticmethod
    def _production(
        observation: DayObservation, orders: tuple[CustomerOrder, ...]
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles = 2_000
        lines: list[ProductionLine] = []
        ordered = sorted(
            orders,
            key=lambda order: (
                order.due_day < observation.day,
                order.due_day,
                -(order.unit_price * order.quantity),
                order.order_id,
            ),
        )
        for order in ordered:
            required = max(0, order.quantity - pcs.get(order.product_id, 0))
            pcs[order.product_id] = max(
                0, pcs.get(order.product_id, 0) - order.quantity
            )
            bom, unit_cycles = PRODUCTS[order.product_id]
            if (
                required == 0
                or required * unit_cycles > cycles
                or any(components.get(component, 0) < required for component in bom)
            ):
                continue
            for component in bom:
                components[component] -= required
            cycles -= required * unit_cycles
            lines.append(
                ProductionLine(
                    f"pl-{observation.day}-{len(lines)}",
                    len(lines),
                    order.product_id,
                    required,
                )
            )
        return tuple(lines[:16])

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
            lines.append(
                DeliveryLine(
                    f"dl-{observation.day}-{len(lines)}",
                    len(lines),
                    order.order_id,
                    order.quantity,
                )
            )
        return tuple(lines[:16])

    @staticmethod
    def _position(observation: DayObservation) -> int:
        return sum(
            item.quantity
            for item in observation.component_inventory
            + observation.pc_inventory
            + observation.pending_inventory
        ) + sum(
            max(0, order.quantity - order.delivered_quantity)
            for order in observation.supplier_orders
        )
