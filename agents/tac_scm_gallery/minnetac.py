"""Independently authored MinneTAC-inspired evaluator and regime coordinator."""

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

from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol


@dataclass(frozen=True, slots=True)
class RegimePolicy:
    name: str
    component_buffer: int
    finished_buffer: int
    maximum_position: int
    maximum_component_position: int
    maximum_open_customer_units: int
    debt_floor: int
    minimum_margin_ppm: int
    supplier_cap_ppm: int
    offer_limit: int
    lead_times: tuple[int, ...]


SUPPLY_TIGHT = RegimePolicy(
    "supply-tight", 2, 0, 140, 18, 8, -55_000_000_000, 220_000, 900_000, 2, (3, 8, 16)
)
BALANCED = RegimePolicy(
    "balanced", 6, 2, 210, 30, 18, -85_000_000_000, 120_000, 1_000_000, 4, (3, 7, 14, 24)
)
DEMAND_TIGHT = RegimePolicy(
    "demand-tight", 1, 0, 120, 16, 24, -45_000_000_000, 55_000, 850_000, 6, (5, 12)
)
ENDGAME = RegimePolicy(
    "endgame", 0, 0, 80, 8, 32, -30_000_000_000, 10_000, 780_000, 8, (2,)
)
POLICIES = {policy.name: policy for policy in (SUPPLY_TIGHT, BALANCED, DEMAND_TIGHT, ENDGAME)}
REGIME_CODES = {"supply-tight": "s", "balanced": "b", "demand-tight": "d", "endgame": "e"}


@dataclass(frozen=True, slots=True)
class Evaluations:
    demand_pressure_ppm: int
    component_cover_ppm: int
    shortage_units: int
    urgent_units: int
    factory_load_ppm: int
    lagged_yield_ppm: int
    cash_after_commitments: int
    feasible_units: int


@dataclass(frozen=True, slots=True)
class RegimeSignal:
    candidate: str
    selected: str
    previous_candidate: str
    evaluations: Evaluations
    immediate: bool


def _inventory(records: tuple[InventoryItem, ...]) -> dict[str, int]:
    values: dict[str, int] = {}
    for record in records:
        values[record.item_id] = values.get(record.item_id, 0) + record.quantity
    return values


def _open_orders(observation: DayObservation) -> tuple[CustomerOrder, ...]:
    return tuple(
        sorted(
            (order for order in observation.active_customer_orders if order.status == "open"),
            key=lambda order: (order.due_day, order.order_id),
        )
    )


def _nominal_product(product_id: str) -> int:
    return sum(BASE_MICROCREDITS[component] for component in PRODUCTS[product_id][0])


def _component_obligations(observation: DayObservation) -> dict[str, int]:
    pcs = _inventory(observation.pc_inventory)
    result = {component: 0 for component in SUPPLIERS}
    for order in _open_orders(observation):
        uncovered = max(0, order.quantity - pcs.get(order.product_id, 0))
        pcs[order.product_id] = max(0, pcs.get(order.product_id, 0) - order.quantity)
        for component in PRODUCTS[order.product_id][0]:
            result[component] += uncovered
    return result


def _dated_supply(observation: DayObservation, *, include_rfqs: bool = True) -> dict[str, int]:
    result = _inventory(observation.component_inventory)
    for item in observation.pending_inventory:
        if item.item_id in SUPPLIERS:
            result[item.item_id] = result.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        result[order.component_id] = result.get(order.component_id, 0) + max(
            0, order.quantity - order.delivered_quantity
        )
    if include_rfqs:
        for request in observation.supplier_rfqs:
            result[request.component_id] = result.get(request.component_id, 0) + request.quantity
    return result


def _position(observation: DayObservation) -> int:
    return (
        sum(item.quantity for item in observation.component_inventory + observation.pc_inventory + observation.pending_inventory)
        + sum(max(0, order.quantity - order.delivered_quantity) for order in observation.supplier_orders)
        + sum(request.quantity for request in observation.supplier_rfqs)
    )


def _recent_rfq_units(history: PublicHistoryProtocol, *, exclude_current: bool = False) -> tuple[int, ...]:
    observations = history.observations[:-1] if exclude_current else history.observations
    return tuple(sum(rfq.quantity for rfq in row.customer_rfqs) for row in observations[-8:])


def evaluate_public_state(observation: DayObservation, history: PublicHistoryProtocol) -> Evaluations:
    obligations = _component_obligations(observation)
    supply = _dated_supply(observation)
    required = sum(obligations.values())
    available = sum(min(supply.get(component, 0), max(1, obligations[component])) for component in SUPPLIERS)
    component_cover = 1_500_000 if required == 0 else min(1_500_000, available * 1_000_000 // required)
    shortage = sum(max(0, obligations[component] - supply.get(component, 0)) for component in SUPPLIERS)
    current_demand = sum(rfq.quantity for rfq in observation.customer_rfqs)
    prior = _recent_rfq_units(history, exclude_current=True)
    prior_mean = sum(prior) // len(prior) if prior else max(1, current_demand)
    demand_pressure = min(2_000_000, current_demand * 1_000_000 // max(1, prior_mean))
    orders = _open_orders(observation)
    urgent = sum(order.quantity for order in orders if order.due_day <= observation.day + 3)
    cycles = sum(order.quantity * PRODUCTS[order.product_id][1] for order in orders)
    factory_load = min(2_000_000, cycles * 1_000_000 // 2_000)
    historical_rfqs = sum(_recent_rfq_units(history))
    historical_orders = sum(
        order.quantity
        for row in history.observations[-8:]
        for order in row.active_customer_orders
        if order.status == "open"
    )
    lagged_yield = min(1_000_000, historical_orders * 1_000_000 // max(1, historical_rfqs))
    committed = sum(
        max(0, order.quantity - order.delivered_quantity) * order.unit_price
        for order in observation.supplier_orders
    )
    components = _inventory(observation.component_inventory)
    feasible = sum(
        min(components.get(component, 0) for component in PRODUCTS[product][0])
        for product in PRODUCTS
    ) // 4
    return Evaluations(
        demand_pressure,
        component_cover,
        shortage,
        urgent,
        factory_load,
        lagged_yield,
        observation.cash_microcredits - committed,
        feasible,
    )


def _candidate(day: int, values: Evaluations) -> tuple[str, bool]:
    if day >= 190:
        return "endgame", True
    if (
        values.shortage_units >= 8
        or values.component_cover_ppm < 850_000
        or values.urgent_units >= 8
        or values.factory_load_ppm >= 1_050_000
    ):
        return "supply-tight", False
    if values.demand_pressure_ppm <= 780_000 and values.component_cover_ppm >= 1_000_000:
        return "demand-tight", False
    return "balanced", False


def select_regime(observation: DayObservation, history: PublicHistoryProtocol) -> RegimeSignal:
    values = evaluate_public_state(observation, history)
    candidate, immediate = _candidate(observation.day, values)
    previous_candidate = "balanced"
    prior = history.observations[:-1]
    if prior:
        shadow = type("ShadowHistory", (), {"observations": tuple(prior), "features": history.features[:-1]})()
        previous_values = evaluate_public_state(prior[-1], shadow)
        previous_candidate = _candidate(prior[-1].day, previous_values)[0]
    selected = candidate if immediate or candidate == previous_candidate else "balanced"
    return RegimeSignal(candidate, selected, previous_candidate, values, immediate)


def _rfq_id(regime: str, component: str, day: int, lead: int) -> str:
    return f"mt-{REGIME_CODES[regime]}-{component}-{day:03d}-{lead:02d}"


def _valid_request(request_id: str, component: str) -> bool:
    if not request_id.startswith("mt-"):
        return False
    body, separator, lead = request_id[3:].rpartition("-")
    intent, day_separator, day = body.rpartition("-")
    regime, component_separator, encoded_component = intent.partition("-")
    return bool(
        separator
        and day_separator
        and component_separator
        and lead.isdigit()
        and day.isdigit()
        and regime in REGIME_CODES.values()
        and encoded_component == component
    )


class MinneTacRegimeKernel:
    """Public evaluator graph with a coordinated, hysteretic regime policy."""

    strategy_id = "minnetac-regime-v1"

    def __init__(self, *, stationary_balanced: bool = False) -> None:
        self.stationary_balanced = stationary_balanced
        if stationary_balanced:
            self.strategy_id = "minnetac-stationary-balanced-v1"

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
                DecisionExplanation(self.strategy_id, "terminal", "No new commitments on the final day.", ("endgame",)),
            )
        signal = select_regime(observation, history)
        regime = "balanced" if self.stationary_balanced else signal.selected
        policy = POLICIES[regime]
        orders = _open_orders(observation)
        deliveries = self._deliveries(observation, orders)
        production = self._production(observation, orders, policy, history)
        supplier_orders = self._supplier_orders(observation, policy)
        supplier_rfqs = self._supplier_rfqs(observation, policy, history)
        customer_offers = self._customer_offers(observation, orders, policy, signal.evaluations)
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            customer_offers,
            supplier_rfqs,
            supplier_orders,
            production,
            deliveries,
        )
        values = signal.evaluations
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                regime,
                "Compose public evaluators into one feasible procurement, sales, production, and delivery posture.",
                (
                    f"candidate-{signal.candidate}",
                    f"previous-{signal.previous_candidate}",
                    "immediate-endgame" if signal.immediate else "two-day-hysteresis",
                    "behavior-informed-bounded",
                ),
                (
                    ("demand_pressure_ppm", values.demand_pressure_ppm),
                    ("component_cover_ppm", values.component_cover_ppm),
                    ("shortage_units", values.shortage_units),
                    ("urgent_units", values.urgent_units),
                    ("factory_load_ppm", values.factory_load_ppm),
                    ("lagged_yield_ppm", values.lagged_yield_ppm),
                    ("cash_after_commitments", values.cash_after_commitments),
                    ("feasible_units", values.feasible_units),
                    ("maximum_position_units", policy.maximum_position),
                    ("customer_offers", len(customer_offers)),
                    ("supplier_rfqs", len(supplier_rfqs)),
                    ("supplier_orders", len(supplier_orders)),
                ),
            ),
        )

    @staticmethod
    def _forecast_products(history: PublicHistoryProtocol) -> dict[str, int]:
        forecast = {product: 0 for product in PRODUCTS}
        observations = history.observations[-8:]
        for row in observations:
            for rfq in row.customer_rfqs:
                forecast[rfq.product_id] += rfq.quantity
        divisor = max(1, len(observations) * 4)
        return {product: quantity // divisor for product, quantity in forecast.items()}

    def _supplier_rfqs(
        self, observation: DayObservation, policy: RegimePolicy, history: PublicHistoryProtocol
    ) -> tuple[SupplierRfqCommand, ...]:
        if observation.day >= 205 or policy.name == "endgame" or observation.cash_microcredits <= policy.debt_floor:
            return ()
        obligations = _component_obligations(observation)
        forecast = self._forecast_products(history)
        wanted = dict(obligations)
        for product, quantity in forecast.items():
            for component in PRODUCTS[product][0]:
                wanted[component] += quantity
        supply = _dated_supply(observation)
        position = _position(observation)
        candidates: list[tuple[int, str, int, int, int]] = []
        for index, component in enumerate(sorted(SUPPLIERS)):
            target = min(policy.maximum_component_position, wanted[component] + policy.component_buffer)
            shortage = max(0, target - supply.get(component, 0))
            if shortage == 0:
                continue
            lead = policy.lead_times[(observation.day + index) % len(policy.lead_times)]
            cap = BASE_MICROCREDITS[component] * policy.supplier_cap_ppm // 1_000_000
            candidates.append((-shortage, component, min(12, shortage), min(219, observation.day + lead), cap))
        requests: list[SupplierRfqCommand] = []
        for _, component, wanted_units, due_day, cap in sorted(candidates):
            quantity = min(wanted_units, policy.maximum_position - position - sum(row.quantity for row in requests))
            if quantity <= 0:
                continue
            requests.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(requests)}",
                    _rfq_id(policy.name, component, observation.day, due_day - observation.day),
                    min(SUPPLIERS[component]),
                    component,
                    quantity,
                    due_day,
                    cap,
                )
            )
            if len(requests) == 8:
                break
        return tuple(requests)

    @staticmethod
    def _supplier_orders(observation: DayObservation, policy: RegimePolicy) -> tuple[SupplierOrderCommand, ...]:
        if observation.day >= 210 or policy.name == "endgame":
            return ()
        requests = {request.rfq_id: request for request in observation.supplier_rfqs}
        groups: dict[str, list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            request = requests.get(offer.rfq_id)
            if (
                request is None
                or not _valid_request(request.client_rfq_id, offer.component_id)
                or offer.kind in {"none", "probe"}
                or offer.quantity <= 0
                or offer.unit_price > (request.max_unit_price or 0)
                or offer.unit_price > BASE_MICROCREDITS[offer.component_id] * policy.supplier_cap_ppm // 1_000_000
            ):
                continue
            groups.setdefault(offer.alternative_group_id, []).append(offer)
        committed = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        position = _position(observation)
        spend = 0
        units = 0
        result: list[SupplierOrderCommand] = []
        for group in sorted(groups):
            offer = min(groups[group], key=lambda row: (row.unit_price, row.due_day, -row.quantity, row.offer_id))
            cost = offer.quantity * offer.unit_price
            if position + units + offer.quantity > policy.maximum_position:
                continue
            if observation.cash_microcredits - committed - spend - cost < policy.debt_floor:
                continue
            result.append(SupplierOrderCommand(f"so-{observation.day}-{len(result)}", offer.offer_id))
            spend += cost
            units += offer.quantity
        return tuple(result)

    @staticmethod
    def _customer_offers(
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        policy: RegimePolicy,
        evaluations: Evaluations,
    ) -> tuple[CustomerOfferCommand, ...]:
        if observation.day >= 214:
            return ()
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        for item in observation.pending_inventory:
            if item.item_id in SUPPLIERS:
                components[item.item_id] = components.get(item.item_id, 0) + item.quantity
        open_units = sum(order.quantity for order in orders)
        candidates: list[tuple[int, str, CustomerRfq, int]] = []
        yield_adjustment = max(-40_000, min(60_000, (400_000 - evaluations.lagged_yield_ppm) // 4))
        for rfq in observation.customer_rfqs:
            if not 3 <= rfq.due_day - observation.day <= 13:
                continue
            if open_units + rfq.quantity > policy.maximum_open_customer_units:
                continue
            required = max(0, rfq.quantity - pcs.get(rfq.product_id, 0))
            bom, cycles = PRODUCTS[rfq.product_id]
            if required * cycles > 2_000 or any(components.get(component, 0) < required for component in bom):
                continue
            nominal = _nominal_product(rfq.product_id)
            margin = max(0, policy.minimum_margin_ppm + yield_adjustment)
            floor = nominal * (1_000_000 + margin) // 1_000_000
            price = min(
                rfq.unit_reserve,
                max(
                    floor,
                    rfq.unit_reserve
                    * (820_000 if policy.name in {"demand-tight", "endgame"} else 910_000)
                    // 1_000_000,
                ),
            )
            contribution = (price - nominal) * rfq.quantity
            if price >= floor and contribution >= 0:
                candidates.append((-contribution, rfq.rfq_id, rfq, price))
        cycles_left = 2_000
        result: list[CustomerOfferCommand] = []
        for _, _, rfq_value, price in sorted(candidates):
            rfq = rfq_value
            required = max(0, rfq.quantity - pcs.get(rfq.product_id, 0))
            covered = rfq.quantity - required
            pcs[rfq.product_id] = max(0, pcs.get(rfq.product_id, 0) - covered)
            bom, cycles = PRODUCTS[rfq.product_id]
            if required * cycles > cycles_left or any(components.get(component, 0) < required for component in bom):
                continue
            for component in bom:
                components[component] -= required
            cycles_left -= required * cycles
            result.append(CustomerOfferCommand(f"co-{observation.day}-{len(result)}", rfq.rfq_id, price))
            open_units += rfq.quantity
            if len(result) == policy.offer_limit:
                break
        return tuple(result)

    def _production(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        policy: RegimePolicy,
        history: PublicHistoryProtocol,
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles_left = 2_000
        result: list[ProductionLine] = []
        ranked = sorted(
            orders,
            key=lambda order: (
                -((order.unit_price - _nominal_product(order.product_id)) * order.quantity),
                order.due_day,
                order.order_id,
            ),
        )
        for order in ranked:
            required = max(0, order.quantity - pcs.get(order.product_id, 0))
            pcs[order.product_id] = max(0, pcs.get(order.product_id, 0) - order.quantity)
            bom, cycles = PRODUCTS[order.product_id]
            if required <= 0 or required * cycles > cycles_left or any(components.get(component, 0) < required for component in bom):
                continue
            for component in bom:
                components[component] -= required
            cycles_left -= required * cycles
            result.append(ProductionLine(f"pl-{observation.day}-{len(result)}", len(result), order.product_id, required))
        if policy.finished_buffer and observation.day < 185:
            forecast = self._forecast_products(history)
            for product in sorted(PRODUCTS, key=lambda item: (-forecast[item], item)):
                bom, cycles = PRODUCTS[product]
                quantity = min(
                    max(0, min(policy.finished_buffer, forecast[product]) - pcs.get(product, 0)),
                    cycles_left // cycles,
                    *[components.get(component, 0) for component in bom],
                )
                if quantity <= 0:
                    continue
                result.append(ProductionLine(f"pl-{observation.day}-{len(result)}", len(result), product, quantity))
                break
        return tuple(result)

    @staticmethod
    def _deliveries(observation: DayObservation, orders: tuple[CustomerOrder, ...]) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        ranked = sorted(
            orders,
            key=lambda order: (
                -(order.unit_price + order.daily_penalty * max(1, observation.day + 1 - order.due_day)),
                order.due_day,
                order.order_id,
            ),
        )
        result: list[DeliveryLine] = []
        for order in ranked:
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            result.append(DeliveryLine(f"dl-{observation.day}-{len(result)}", len(result), order.order_id, order.quantity))
        return tuple(result)
