"""Bounded CrocodileAgent-inspired mixed-horizon supply control."""

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


BOOTSTRAP_END = 4
SCHEDULED_LONG_END = 36
WINDDOWN_START = 184
SHORT_COVER_DAYS = 8
DISTANCE_HORIZON_DAYS = 30
MAX_POSITION = 224
MAX_COMPONENT_POSITION = 36
MAX_OPEN_CUSTOMER_UNITS = 32
DEBT_FLOOR = -90_000_000_000
DAILY_RFQ_LIMIT = 8


@dataclass(frozen=True, slots=True)
class HorizonSignal:
    component_id: str
    target_units: int
    effective_supply_units: int
    future_exposure_units: int
    requested_units: int
    horizon: str
    due_day: int
    max_unit_price: int


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


def game_stage(day: int, open_units: int) -> str:
    if day >= WINDDOWN_START:
        return "endgame"
    if day <= 14:
        return "start"
    if open_units >= 24:
        return "high-demand"
    return "normal"


def procurement_stage(day: int) -> str:
    if day <= BOOTSTRAP_END:
        return "bootstrap"
    if day <= SCHEDULED_LONG_END:
        return "scheduled-long"
    if day < WINDDOWN_START:
        return "responsive"
    return "winddown"


def weighted_supply(observation: DayObservation, component_id: str) -> tuple[int, int]:
    """Return distance-weighted near supply and unweighted total exposure."""

    on_hand = _inventory(observation.component_inventory).get(component_id, 0)
    pending = _inventory(observation.pending_inventory).get(component_id, 0)
    effective_milli = (on_hand + pending) * 1_000
    future = on_hand + pending
    for order in observation.supplier_orders:
        if order.component_id != component_id:
            continue
        remaining = max(0, order.quantity - order.delivered_quantity)
        lead = max(0, order.due_day - observation.day)
        weight = max(0, DISTANCE_HORIZON_DAYS - lead)
        effective_milli += remaining * weight * 1_000 // DISTANCE_HORIZON_DAYS
        future += remaining
    for request in observation.supplier_rfqs:
        if request.component_id != component_id:
            continue
        lead = max(0, request.due_day - observation.day)
        weight = max(0, DISTANCE_HORIZON_DAYS - lead)
        effective_milli += request.quantity * weight * 1_000 // DISTANCE_HORIZON_DAYS
        future += request.quantity
    return effective_milli // 1_000, future


def _component_obligations(observation: DayObservation) -> dict[str, int]:
    pcs = _inventory(observation.pc_inventory)
    demand = {component: 0 for component in SUPPLIERS}
    for order in _open_orders(observation):
        covered = min(pcs.get(order.product_id, 0), order.quantity)
        pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
        needed = order.quantity - covered
        for component in PRODUCTS[order.product_id][0]:
            demand[component] += needed
    return demand


def _recent_component_demand(
    history: PublicHistoryProtocol,
) -> dict[str, int]:
    totals = {component: 0 for component in SUPPLIERS}
    observations = history.observations[-5:]
    if not observations:
        return totals
    for observation in observations:
        for rfq in observation.customer_rfqs:
            for component in PRODUCTS[rfq.product_id][0]:
                totals[component] += rfq.quantity
    divisor = max(1, len(observations) * 6)
    return {component: min(12, quantity // divisor) for component, quantity in totals.items()}


def _position(observation: DayObservation) -> int:
    return sum(
        item.quantity
        for item in observation.component_inventory
        + observation.pc_inventory
        + observation.pending_inventory
    ) + sum(
        max(0, order.quantity - order.delivered_quantity)
        for order in observation.supplier_orders
    ) + sum(request.quantity for request in observation.supplier_rfqs)


def _rfq_id(horizon: str, component_id: str, day: int) -> str:
    return f"croc-{horizon}-{component_id}-{day:03d}"


def _parse_rfq_id(value: str) -> tuple[str, str] | None:
    if not value.startswith("croc-"):
        return None
    intent, separator, day = value[5:].rpartition("-")
    horizon, component_separator, component = intent.partition("-")
    if not separator or not day.isdigit() or not component_separator:
        return None
    if horizon not in {"bootstrap", "long", "short", "medium", "alarm"}:
        return None
    if component not in SUPPLIERS:
        return None
    return horizon, component


def _price_cap(component: str, horizon: str) -> int:
    ppm = {
        "long": 680_000,
        "bootstrap": 880_000,
        "medium": 900_000,
        "short": 1_000_000,
        "alarm": 1_120_000,
    }[horizon]
    return BASE_MICROCREDITS[component] * ppm // 1_000_000


class CrocodileMixedHorizonKernel:
    """Publication-inspired three-horizon procurement and staged trading."""

    strategy_id = "crocodile-mixed-v1"

    def __init__(self, *, variant: str = "full") -> None:
        if variant not in {"full", "short-only", "long-only"}:
            raise ValueError("unknown Crocodile-inspired variant")
        self.variant = variant
        if variant != "full":
            self.strategy_id = f"crocodile-{variant}-v1"

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: PublicHistoryProtocol,
    ) -> KernelDecision:
        del features
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
        orders = _open_orders(observation)
        open_units = sum(order.quantity for order in orders)
        sales_stage = game_stage(observation.day, open_units)
        signals = self._horizon_signals(observation, history)
        supplier_rfqs = self._supplier_rfqs(observation, signals)
        supplier_orders = self._supplier_orders(observation)
        offers = self._customer_offers(observation, orders, sales_stage)
        production = self._production(observation, orders, history)
        deliveries = self._deliveries(observation, orders)
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
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                sales_stage,
                "Blend bounded dated supply with public demand and order obligations.",
                (
                    procurement_stage(observation.day),
                    self.variant,
                    "distance-weighted-supply",
                    "orders-before-stock",
                ),
                (
                    ("target_units", sum(signal.target_units for signal in signals)),
                    (
                        "effective_supply_units",
                        sum(signal.effective_supply_units for signal in signals),
                    ),
                    (
                        "future_exposure_units",
                        sum(signal.future_exposure_units for signal in signals),
                    ),
                    ("requested_units", sum(request.quantity for request in supplier_rfqs)),
                    ("short_horizon_requests", sum(signal.horizon in {"short", "alarm"} and signal.requested_units > 0 for signal in signals)),
                    ("medium_horizon_requests", sum(signal.horizon == "medium" and signal.requested_units > 0 for signal in signals)),
                    ("long_horizon_requests", sum(signal.horizon in {"bootstrap", "long"} and signal.requested_units > 0 for signal in signals)),
                    ("cash_headroom_microcredits", observation.cash_microcredits - DEBT_FLOOR),
                    ("customer_offers", len(offers)),
                    ("supplier_orders", len(supplier_orders)),
                ),
            ),
        )

    def _horizon_signals(
        self,
        observation: DayObservation,
        history: PublicHistoryProtocol,
    ) -> tuple[HorizonSignal, ...]:
        obligations = _component_obligations(observation)
        recent = _recent_component_demand(history)
        stage = procurement_stage(observation.day)
        signals: list[HorizonSignal] = []
        long_days = {6, 12, 18, 24, 30, 36}
        for index, component in enumerate(sorted(SUPPLIERS)):
            effective, future = weighted_supply(observation, component)
            target = min(MAX_COMPONENT_POSITION, obligations[component])
            horizon = "short"
            due_day = min(219, observation.day + 5)
            if stage == "bootstrap" and self.variant != "short-only":
                target = max(target, 4)
                horizon = "bootstrap"
                due_day = min(219, observation.day + 4 + index % 3)
            elif (
                observation.day in long_days
                and self.variant != "short-only"
                and future < 12
            ):
                target = max(target, 12)
                horizon = "long"
                due_day = min(219, observation.day + 42 + index * 2)
            elif self.variant != "long-only":
                buffer = 2 if stage == "winddown" else 5
                target = min(
                    MAX_COMPONENT_POSITION,
                    max(target, obligations[component] + buffer + min(SHORT_COVER_DAYS, recent[component])),
                )
                shortage = max(0, target - effective)
                if obligations[component] > effective:
                    horizon = "alarm"
                    due_day = min(219, observation.day + 3)
                elif shortage > 8:
                    horizon = "medium"
                    due_day = min(219, observation.day + 14)
            requested = min(12, max(0, target - effective))
            if stage == "winddown":
                remaining = max(0, 208 - observation.day)
                target = min(target, obligations[component] + remaining // 8)
                requested = min(requested, max(0, target - effective))
            if self.variant == "long-only" and horizon not in {"bootstrap", "long"}:
                requested = 0
            if self.variant == "short-only" and horizon in {"bootstrap", "long"}:
                requested = 0
            if future >= MAX_COMPONENT_POSITION:
                requested = 0
            signals.append(
                HorizonSignal(
                    component,
                    target,
                    effective,
                    future,
                    requested,
                    horizon,
                    due_day,
                    _price_cap(component, horizon),
                )
            )
        return tuple(signals)

    @staticmethod
    def _supplier_rfqs(
        observation: DayObservation,
        signals: tuple[HorizonSignal, ...],
    ) -> tuple[SupplierRfqCommand, ...]:
        if observation.day >= 208 or observation.cash_microcredits <= DEBT_FLOOR:
            return ()
        position = _position(observation)
        requests: list[SupplierRfqCommand] = []
        for signal in sorted(
            signals,
            key=lambda item: (
                item.horizon not in {"alarm", "short"},
                -item.requested_units,
                item.component_id,
            ),
        ):
            quantity = min(
                signal.requested_units,
                MAX_POSITION - position - sum(item.quantity for item in requests),
                MAX_COMPONENT_POSITION - signal.future_exposure_units,
            )
            if quantity <= 0:
                continue
            requests.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(requests)}",
                    _rfq_id(signal.horizon, signal.component_id, observation.day),
                    min(SUPPLIERS[signal.component_id]),
                    signal.component_id,
                    quantity,
                    signal.due_day,
                    signal.max_unit_price,
                )
            )
            if len(requests) == DAILY_RFQ_LIMIT:
                break
        return tuple(requests)

    @staticmethod
    def _supplier_orders(
        observation: DayObservation,
    ) -> tuple[SupplierOrderCommand, ...]:
        if observation.day >= 212:
            return ()
        requests = {request.rfq_id: request for request in observation.supplier_rfqs}
        grouped: dict[str, list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            request = requests.get(offer.rfq_id)
            parsed = None if request is None else _parse_rfq_id(request.client_rfq_id)
            if (
                request is None
                or parsed is None
                or offer.kind in {"none", "probe"}
                or offer.quantity <= 0
                or offer.component_id != parsed[1]
                or offer.unit_price > (request.max_unit_price or 0)
            ):
                continue
            grouped.setdefault(offer.alternative_group_id, []).append(offer)
        outstanding = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        component_exposure = {
            component: weighted_supply(observation, component)[1]
            for component in SUPPLIERS
        }
        position = _position(observation)
        spend = 0
        units = 0
        selected: list[SupplierOrderCommand] = []
        for group in sorted(grouped):
            offer = min(
                grouped[group],
                key=lambda item: (item.unit_price, item.due_day, -item.quantity, item.offer_id),
            )
            cost = offer.quantity * offer.unit_price
            if (
                component_exposure[offer.component_id] + offer.quantity > MAX_COMPONENT_POSITION
                or position + units + offer.quantity > MAX_POSITION
                or observation.cash_microcredits - outstanding - spend - cost < DEBT_FLOOR
            ):
                continue
            selected.append(
                SupplierOrderCommand(
                    f"so-{observation.day}-{len(selected)}", offer.offer_id
                )
            )
            component_exposure[offer.component_id] += offer.quantity
            units += offer.quantity
            spend += cost
        return tuple(selected)

    @staticmethod
    def _recent_price(observation: DayObservation, product_id: str) -> int | None:
        if observation.daily_price_report is not None:
            for row in observation.daily_price_report.products:
                if row.product_id == product_id:
                    values = [value for value in (row.minimum, row.maximum) if value is not None]
                    if values:
                        return sum(values) // len(values)
        if observation.market_report is not None:
            for row in observation.market_report.products:
                if row.product_id == product_id:
                    return row.mean_price
        return None

    def _customer_offers(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        stage: str,
    ) -> tuple[CustomerOfferCommand, ...]:
        if observation.day >= 207:
            return ()
        open_units = sum(order.quantity for order in orders)
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        limit = {"start": 1, "normal": 4, "high-demand": 2, "endgame": 4}[stage]
        margin_ppm = {"start": 220_000, "normal": 120_000, "high-demand": 180_000, "endgame": 40_000}[stage]
        candidates: list[tuple[int, int, str, CustomerRfq, int]] = []
        for rfq in observation.customer_rfqs:
            lead = rfq.due_day - observation.day
            if not 4 <= lead <= 12 or open_units + rfq.quantity > MAX_OPEN_CUSTOMER_UNITS:
                continue
            covered = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            required = rfq.quantity - covered
            bom, cycles = PRODUCTS[rfq.product_id]
            if any(components.get(component, 0) < required for component in bom):
                continue
            if required * cycles > 2_000:
                continue
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * (1_000_000 + margin_ppm) // 1_000_000
            observed = self._recent_price(observation, rfq.product_id)
            anchor = floor if observed is None else max(floor, observed)
            if stage in {"start", "high-demand"}:
                anchor = max(anchor, rfq.unit_reserve * 9 // 10)
            price = min(rfq.unit_reserve, anchor)
            if price < floor:
                continue
            surplus = (price - nominal) * rfq.quantity
            candidates.append((-surplus, rfq.due_day, rfq.rfq_id, rfq, price))
        offers: list[CustomerOfferCommand] = []
        cycles_left = 2_000
        for _, _, _, rfq, price in sorted(candidates):
            covered = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - covered
            required = rfq.quantity - covered
            bom, unit_cycles = PRODUCTS[rfq.product_id]
            if required * unit_cycles > cycles_left or any(components.get(component, 0) < required for component in bom):
                continue
            for component in bom:
                components[component] -= required
            cycles_left -= required * unit_cycles
            offers.append(CustomerOfferCommand(f"co-{observation.day}-{len(offers)}", rfq.rfq_id, price))
            open_units += rfq.quantity
            if len(offers) == limit:
                break
        return tuple(offers)

    @staticmethod
    def _production(
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        history: PublicHistoryProtocol,
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles = 2_000
        lines: list[ProductionLine] = []
        for order in orders:
            covered = min(pcs.get(order.product_id, 0), order.quantity)
            pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
            required = order.quantity - covered
            bom, unit_cycles = PRODUCTS[order.product_id]
            if required <= 0 or required * unit_cycles > cycles or any(components.get(component, 0) < required for component in bom):
                continue
            for component in bom:
                components[component] -= required
            cycles -= required * unit_cycles
            lines.append(ProductionLine(f"pl-{observation.day}-{len(lines)}", len(lines), order.product_id, required))
        if 15 <= observation.day < WINDDOWN_START and cycles:
            demand: dict[str, int] = {product: 0 for product in PRODUCTS}
            for item in history.observations[-5:]:
                for rfq in item.customer_rfqs:
                    demand[rfq.product_id] += rfq.quantity
            product_id = min(PRODUCTS, key=lambda product: (-demand[product], product))
            bom, unit_cycles = PRODUCTS[product_id]
            quantity = min(
                2 - pcs.get(product_id, 0),
                cycles // unit_cycles,
                *[components.get(component, 0) for component in bom],
            )
            if quantity > 0:
                lines.append(ProductionLine(f"pl-{observation.day}-{len(lines)}", len(lines), product_id, quantity))
        return tuple(lines)

    @staticmethod
    def _deliveries(
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
    ) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        lines: list[DeliveryLine] = []
        for order in orders:
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            lines.append(DeliveryLine(f"dl-{observation.day}-{len(lines)}", len(lines), order.order_id, order.quantity))
        return tuple(lines)
