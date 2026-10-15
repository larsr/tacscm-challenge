"""Bounded Botticelli-inspired scenario planning over public observations."""

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
class Scenario:
    name: str
    probability_ppm: int
    demand_ppm: int
    supply_delay_days: int


SCENARIOS = (
    Scenario("low-ontime", 200_000, 700_000, 0),
    Scenario("central-ontime", 400_000, 1_000_000, 0),
    Scenario("high-ontime", 200_000, 1_300_000, 0),
    Scenario("central-delayed", 200_000, 1_000_000, 3),
)
EXPECTED_SCENARIO = Scenario("expected-value", 1_000_000, 1_000_000, 1)
HORIZON_DAYS = 10
MAXIMUM_POSITION = 224
MAXIMUM_COMPONENT_POSITION = 32
MAXIMUM_OPEN_CUSTOMER_UNITS = 36
DEBT_FLOOR = -95_000_000_000


@dataclass(frozen=True, slots=True)
class PlannerEvidence:
    nodes: int
    operation_budget: int
    fallback: bool
    expected_plan_value: int
    expected_value_plan_value: int
    greedy_plan_value: int
    reserved_cycles: int
    supplier_marginal_value: int


def _inventory(records: tuple[InventoryItem, ...]) -> dict[str, int]:
    result: dict[str, int] = {}
    for record in records:
        result[record.item_id] = result.get(record.item_id, 0) + record.quantity
    return result


def _open_orders(observation: DayObservation) -> tuple[CustomerOrder, ...]:
    return tuple(
        sorted(
            (order for order in observation.active_customer_orders if order.status == "open"),
            key=lambda order: (order.due_day, order.order_id),
        )
    )


def _nominal_product(product_id: str) -> int:
    return sum(BASE_MICROCREDITS[component] for component in PRODUCTS[product_id][0])


def _position(observation: DayObservation) -> int:
    return (
        sum(item.quantity for item in observation.component_inventory + observation.pc_inventory + observation.pending_inventory)
        + sum(max(0, order.quantity - order.delivered_quantity) for order in observation.supplier_orders)
        + sum(request.quantity for request in observation.supplier_rfqs)
    )


def _component_need(observation: DayObservation) -> dict[str, int]:
    pcs = _inventory(observation.pc_inventory)
    need = {component: 0 for component in SUPPLIERS}
    for order in _open_orders(observation):
        uncovered = max(0, order.quantity - pcs.get(order.product_id, 0))
        pcs[order.product_id] = max(0, pcs.get(order.product_id, 0) - order.quantity)
        for component in PRODUCTS[order.product_id][0]:
            need[component] += uncovered
    return need


def _supply(observation: DayObservation) -> dict[str, int]:
    supply = _inventory(observation.component_inventory)
    for item in observation.pending_inventory:
        if item.item_id in SUPPLIERS:
            supply[item.item_id] = supply.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        supply[order.component_id] = supply.get(order.component_id, 0) + max(
            0, order.quantity - order.delivered_quantity
        )
    return supply


def _forecast_products(history: PublicHistoryProtocol) -> dict[str, int]:
    result = {product: 0 for product in PRODUCTS}
    observations = history.observations[-5:]
    for row in observations:
        for rfq in row.customer_rfqs:
            result[rfq.product_id] += rfq.quantity
    divisor = max(1, len(observations) * 6)
    return {product: quantity // divisor for product, quantity in result.items()}


def _request_id(component: str, day: int) -> str:
    return f"bo-{component}-{day:03d}"


def _request_component(value: str) -> str | None:
    if not value.startswith("bo-"):
        return None
    component, separator, day = value[3:].rpartition("-")
    return component if separator and day.isdigit() and component in SUPPLIERS else None


def _scenarios(mode: str) -> tuple[Scenario, ...]:
    if mode == "scenario":
        return SCENARIOS
    if mode == "expected-value":
        return (EXPECTED_SCENARIO,)
    return ()


def _future_capacity(day: int) -> tuple[int, ...]:
    remaining = min(HORIZON_DAYS, 219 - day)
    return tuple(2_000 * (remaining - offset) // max(1, remaining) for offset in range(remaining))


def _plan_value(
    observation: DayObservation,
    history: PublicHistoryProtocol,
    scenarios: tuple[Scenario, ...],
    accepted: tuple[SupplierOffer, ...] = (),
) -> tuple[int, int, int]:
    forecast = _forecast_products(history)
    supply = _supply(observation)
    purchase_cost = 0
    for offer in accepted:
        supply[offer.component_id] = supply.get(offer.component_id, 0) + offer.quantity
        purchase_cost += offer.quantity * offer.unit_price
    capacities = _future_capacity(observation.day)
    reserved_cycles = sum(capacities)
    nodes = 0
    if not scenarios:
        feasible = sum(
            min(supply.get(component, 0) for component in PRODUCTS[product][0])
            for product in PRODUCTS
        )
        return feasible * 100_000_000 - purchase_cost, nodes, reserved_cycles
    expected = 0
    for scenario in scenarios:
        nodes += 1
        value = -purchase_cost
        cycles = reserved_cycles
        local_supply = dict(supply)
        for product in sorted(PRODUCTS, key=lambda item: (-forecast[item], item)):
            demand = forecast[product] * scenario.demand_ppm // 1_000_000
            bom, unit_cycles = PRODUCTS[product]
            feasible = min(demand, cycles // unit_cycles, *[local_supply.get(component, 0) for component in bom])
            if feasible <= 0:
                continue
            for component in bom:
                local_supply[component] -= feasible
            cycles -= feasible * unit_cycles
            sale_value = _nominal_product(product) * 1_150_000 // 1_000_000
            value += feasible * sale_value
        late_units = sum(
            offer.quantity
            for offer in accepted
            if offer.due_day + scenario.supply_delay_days > observation.day + HORIZON_DAYS
        )
        residual = sum(local_supply.values())
        value -= late_units * 120_000_000 + residual * 8_000_000
        expected += scenario.probability_ppm * value // 1_000_000
    return expected, nodes, reserved_cycles


class BotticelliScenarioKernel:
    """Exact bounded enumerator with expected-value and greedy ablations."""

    strategy_id = "botticelli-scenario-v1"

    def __init__(self, *, mode: str = "scenario", operation_budget: int = 512) -> None:
        if mode not in {"scenario", "expected-value", "greedy"}:
            raise ValueError("unknown Botticelli-inspired planner mode")
        if operation_budget < 0:
            raise ValueError("operation budget must be non-negative")
        self.mode = mode
        self.operation_budget = operation_budget
        if mode != "scenario":
            self.strategy_id = f"botticelli-{mode}-v1"

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
                DecisionExplanation(self.strategy_id, "terminal", "No future stage remains.", ("endgame",)),
            )
        scenario_value, base_nodes, reserved_cycles = _plan_value(observation, history, SCENARIOS)
        ev_value, ev_nodes, _ = _plan_value(observation, history, (EXPECTED_SCENARIO,))
        greedy_value, _, _ = _plan_value(observation, history, ())
        required_nodes = base_nodes + ev_nodes + len(observation.supplier_offers) * max(1, len(_scenarios(self.mode)))
        fallback = self.operation_budget < required_nodes
        effective_mode = "greedy" if fallback else self.mode
        supplier_orders, supplier_marginal, selection_nodes = self._supplier_orders(
            observation, history, effective_mode
        )
        nodes = required_nodes + selection_nodes
        orders = _open_orders(observation)
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            self._customer_offers(observation, orders, history, effective_mode),
            self._supplier_rfqs(observation, history),
            supplier_orders,
            self._production(observation, orders, history),
            self._deliveries(observation, orders),
        )
        evidence = PlannerEvidence(
            nodes,
            self.operation_budget,
            fallback,
            scenario_value,
            ev_value,
            greedy_value,
            reserved_cycles,
            supplier_marginal,
        )
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                f"{effective_mode}-fallback" if fallback else effective_mode,
                "Choose one nonanticipative action from bounded public demand and supply scenarios.",
                (
                    "ten-day-horizon",
                    "four-public-scenarios",
                    "fixed-operation-budget",
                    "deterministic-greedy-fallback" if fallback else "bounded-enumeration",
                ),
                (
                    ("planner_nodes", evidence.nodes),
                    ("operation_budget", evidence.operation_budget),
                    ("fallback", int(evidence.fallback)),
                    ("scenario_plan_value", evidence.expected_plan_value),
                    ("expected_value_plan_value", evidence.expected_value_plan_value),
                    ("greedy_plan_value", evidence.greedy_plan_value),
                    ("reserved_cycles", evidence.reserved_cycles),
                    ("supplier_marginal_value", evidence.supplier_marginal_value),
                    ("scenario_count", len(_scenarios(effective_mode))),
                ),
            ),
        )

    @staticmethod
    def _supplier_rfqs(observation: DayObservation, history: PublicHistoryProtocol) -> tuple[SupplierRfqCommand, ...]:
        if observation.day >= 205 or observation.cash_microcredits <= DEBT_FLOOR:
            return ()
        need = _component_need(observation)
        forecast = _forecast_products(history)
        for product, quantity in forecast.items():
            for component in PRODUCTS[product][0]:
                need[component] += quantity
        supply = _supply(observation)
        position = _position(observation)
        requests: list[SupplierRfqCommand] = []
        for component in sorted(SUPPLIERS, key=lambda item: (-(need[item] - supply.get(item, 0)), item)):
            target = min(MAXIMUM_COMPONENT_POSITION, need[component] + 5)
            shortage = max(0, target - supply.get(component, 0))
            quantity = min(10, shortage, MAXIMUM_POSITION - position - sum(row.quantity for row in requests))
            if quantity <= 0:
                continue
            lead = 3 + (observation.day + len(requests)) % 10
            requests.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(requests)}",
                    _request_id(component, observation.day),
                    min(SUPPLIERS[component]),
                    component,
                    quantity,
                    min(219, observation.day + lead),
                    BASE_MICROCREDITS[component],
                )
            )
            if len(requests) == 8:
                break
        return tuple(requests)

    def _supplier_orders(
        self, observation: DayObservation, history: PublicHistoryProtocol, mode: str
    ) -> tuple[tuple[SupplierOrderCommand, ...], int, int]:
        if observation.day >= 210:
            return (), 0, 0
        requests = {request.rfq_id: request for request in observation.supplier_rfqs}
        groups: dict[str, list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            request = requests.get(offer.rfq_id)
            if (
                request is None
                or _request_component(request.client_rfq_id) != offer.component_id
                or offer.kind in {"none", "probe"}
                or offer.quantity <= 0
                or offer.unit_price > (request.max_unit_price or 0)
            ):
                continue
            groups.setdefault(offer.alternative_group_id, []).append(offer)
        candidates = tuple(
            min(rows, key=lambda offer: (offer.unit_price, offer.due_day, -offer.quantity, offer.offer_id))
            for _, rows in sorted(groups.items())
        )
        if not candidates:
            return (), 0, 0
        scenarios = _scenarios(mode)
        scored: list[tuple[int, SupplierOffer]] = []
        nodes = 0
        full_value = 0
        if mode != "greedy":
            full_value, nodes, _ = _plan_value(observation, history, scenarios, candidates)
        for index, offer in enumerate(candidates):
            if mode == "greedy":
                marginal = BASE_MICROCREDITS[offer.component_id] * offer.quantity - offer.unit_price * offer.quantity
            else:
                without = candidates[:index] + candidates[index + 1 :]
                value_without, used, _ = _plan_value(observation, history, scenarios, without)
                nodes += used
                marginal = full_value - value_without
            scored.append((-marginal, offer))
        committed = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        position = _position(observation)
        spend = 0
        units = 0
        total_marginal = 0
        selected: list[SupplierOrderCommand] = []
        for negative_marginal, offer in sorted(scored, key=lambda row: (row[0], row[1].offer_id)):
            marginal = -negative_marginal
            cost = offer.quantity * offer.unit_price
            if mode != "greedy" and marginal <= 0:
                continue
            if position + units + offer.quantity > MAXIMUM_POSITION:
                continue
            if observation.cash_microcredits - committed - spend - cost < DEBT_FLOOR:
                continue
            selected.append(SupplierOrderCommand(f"so-{observation.day}-{len(selected)}", offer.offer_id))
            spend += cost
            units += offer.quantity
            total_marginal += marginal
        return tuple(selected), total_marginal, nodes

    @staticmethod
    def _customer_offers(
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        history: PublicHistoryProtocol,
        mode: str,
    ) -> tuple[CustomerOfferCommand, ...]:
        if observation.day >= 207:
            return ()
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        open_units = sum(order.quantity for order in orders)
        scenarios = _scenarios(mode)
        demand_weight = 1_000_000 if not scenarios else sum(
            scenario.probability_ppm * scenario.demand_ppm // 1_000_000 for scenario in scenarios
        )
        candidates: list[tuple[int, str, CustomerRfq, int]] = []
        for rfq in observation.customer_rfqs:
            if not 4 <= rfq.due_day - observation.day <= 12:
                continue
            if open_units + rfq.quantity > MAXIMUM_OPEN_CUSTOMER_UNITS:
                continue
            required = max(0, rfq.quantity - pcs.get(rfq.product_id, 0))
            bom, cycles = PRODUCTS[rfq.product_id]
            if required * cycles > 2_000 or any(components.get(component, 0) < required for component in bom):
                continue
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * 1_100_000 // 1_000_000
            price = min(rfq.unit_reserve, max(floor, rfq.unit_reserve * (880_000 if mode == "scenario" else 840_000) // 1_000_000))
            expected = (price - nominal) * rfq.quantity * demand_weight // 1_000_000
            if price >= floor and expected > 0:
                candidates.append((-expected, rfq.rfq_id, rfq, price))
        cycles_left = 2_000
        result: list[CustomerOfferCommand] = []
        for _, _, rfq, price in sorted(candidates):
            covered = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - covered
            required = rfq.quantity - covered
            bom, cycles = PRODUCTS[rfq.product_id]
            if required * cycles > cycles_left or any(components.get(component, 0) < required for component in bom):
                continue
            for component in bom:
                components[component] -= required
            cycles_left -= required * cycles
            result.append(CustomerOfferCommand(f"co-{observation.day}-{len(result)}", rfq.rfq_id, price))
            open_units += rfq.quantity
            if len(result) == 5:
                break
        return tuple(result)

    @staticmethod
    def _production(
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        history: PublicHistoryProtocol,
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles_left = 2_000
        result: list[ProductionLine] = []
        for order in sorted(orders, key=lambda row: (row.due_day, -row.daily_penalty, row.order_id)):
            required = max(0, order.quantity - pcs.get(order.product_id, 0))
            pcs[order.product_id] = max(0, pcs.get(order.product_id, 0) - order.quantity)
            bom, cycles = PRODUCTS[order.product_id]
            if required <= 0 or required * cycles > cycles_left or any(components.get(component, 0) < required for component in bom):
                continue
            for component in bom:
                components[component] -= required
            cycles_left -= required * cycles
            result.append(ProductionLine(f"pl-{observation.day}-{len(result)}", len(result), order.product_id, required))
        if observation.day < 190 and cycles_left:
            forecast = _forecast_products(history)
            product = min(PRODUCTS, key=lambda item: (-forecast[item], item))
            bom, cycles = PRODUCTS[product]
            quantity = min(2 - pcs.get(product, 0), cycles_left // cycles, *[components.get(component, 0) for component in bom])
            if quantity > 0:
                result.append(ProductionLine(f"pl-{observation.day}-{len(result)}", len(result), product, quantity))
        return tuple(result)

    @staticmethod
    def _deliveries(observation: DayObservation, orders: tuple[CustomerOrder, ...]) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        result: list[DeliveryLine] = []
        for order in sorted(orders, key=lambda row: (-row.daily_penalty, row.due_day, row.order_id)):
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            result.append(DeliveryLine(f"dl-{observation.day}-{len(result)}", len(result), order.order_id, order.quantity))
        return tuple(result)
