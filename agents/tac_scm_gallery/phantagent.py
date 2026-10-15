"""Explicitly approximate PhantAgent-inspired dynamic policy selector."""

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
class ModeProfile:
    name: str
    buffer_units: int
    maximum_position: int
    maximum_component_position: int
    maximum_open_customer_units: int
    debt_floor: int
    minimum_margin_ppm: int
    supplier_cap_ppm: int
    count_near_supply_for_bids: bool
    speculative_pc_units: int
    lead_times: tuple[int, ...]


CONSERVATIVE = ModeProfile(
    "conservative", 3, 80, 12, 8, -40_000_000_000, 200_000, 820_000, False, 0, (4, 12)
)
BALANCED = ModeProfile(
    "balanced", 6, 160, 24, 16, -75_000_000_000, 120_000, 950_000, True, 1, (3, 8, 18)
)
AGGRESSIVE = ModeProfile(
    "aggressive", 10, 256, 36, 32, -110_000_000_000, 80_000, 1_100_000, True, 3, (2, 5, 12, 24)
)
PROFILES = {profile.name: profile for profile in (CONSERVATIVE, BALANCED, AGGRESSIVE)}
MODE_CODES = {"conservative": "c", "balanced": "b", "aggressive": "a"}
CODE_MODES = {code: mode for mode, code in MODE_CODES.items()}


@dataclass(frozen=True, slots=True)
class ModeSignal:
    candidate: str
    selected: str
    previous_candidate: str
    demand_ratio_ppm: int
    shortage_units: int
    factory_load_ppm: int
    profitable_rfqs: int
    cash_headroom_microcredits: int
    emergency: bool


def _inventory(records: tuple[InventoryItem, ...]) -> dict[str, int]:
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


def _component_demand(observation: DayObservation) -> dict[str, int]:
    pcs = _inventory(observation.pc_inventory)
    demand = {component: 0 for component in SUPPLIERS}
    for order in _open_orders(observation):
        covered = min(pcs.get(order.product_id, 0), order.quantity)
        pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
        required = order.quantity - covered
        for component in PRODUCTS[order.product_id][0]:
            demand[component] += required
    return demand


def _supply(observation: DayObservation, *, include_requests: bool = True) -> dict[str, int]:
    supply = _inventory(observation.component_inventory)
    for item in observation.pending_inventory:
        if item.item_id in SUPPLIERS:
            supply[item.item_id] = supply.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        supply[order.component_id] = supply.get(order.component_id, 0) + max(0, order.quantity - order.delivered_quantity)
    if include_requests:
        for request in observation.supplier_rfqs:
            supply[request.component_id] = supply.get(request.component_id, 0) + request.quantity
    return supply


def _position(observation: DayObservation) -> int:
    return sum(
        item.quantity
        for item in observation.component_inventory + observation.pc_inventory + observation.pending_inventory
    ) + sum(max(0, order.quantity - order.delivered_quantity) for order in observation.supplier_orders) + sum(
        request.quantity for request in observation.supplier_rfqs
    )


def _candidate(observation: DayObservation, previous: tuple[DayObservation, ...]) -> tuple[str, tuple[int, int, int, int, int, bool]]:
    demand = _component_demand(observation)
    supply = _supply(observation)
    shortage = sum(max(0, demand[component] - supply.get(component, 0)) for component in SUPPLIERS)
    factory_cycles = sum(
        order.quantity * PRODUCTS[order.product_id][1]
        for order in _open_orders(observation)
    )
    factory_load = min(2_000_000, factory_cycles * 1_000_000 // 2_000)
    current_units = sum(rfq.quantity for rfq in observation.customer_rfqs)
    prior_units = [sum(rfq.quantity for rfq in item.customer_rfqs) for item in previous[-5:]]
    prior_mean = sum(prior_units) // len(prior_units) if prior_units else current_units
    demand_ratio = current_units * 1_000_000 // max(1, prior_mean)
    profitable = sum(
        rfq.due_day - observation.day >= 4
        and rfq.unit_reserve >= _nominal_product(rfq.product_id) * 6 // 5
        for rfq in observation.customer_rfqs
    )
    outstanding = sum(
        max(0, order.quantity - order.delivered_quantity) * order.unit_price
        for order in observation.supplier_orders
    )
    cash_headroom = observation.cash_microcredits - outstanding + 60_000_000_000
    emergency = (
        observation.day >= 190
        or observation.cash_microcredits - outstanding <= -60_000_000_000
        or (shortage >= 12 and factory_load >= 700_000)
        or factory_load >= 1_300_000
    )
    if emergency:
        candidate = "conservative"
    elif (
        observation.day < 175
        and observation.cash_microcredits - outstanding > -25_000_000_000
        and shortage <= 4
        and factory_load <= 650_000
        and demand_ratio >= 900_000
        and profitable >= 2
    ):
        candidate = "aggressive"
    else:
        candidate = "balanced"
    return candidate, (demand_ratio, shortage, factory_load, profitable, cash_headroom, emergency)


def select_mode(observation: DayObservation, history: PublicHistoryProtocol) -> ModeSignal:
    observations = history.observations
    previous = observations[:-1]
    candidate, metrics = _candidate(observation, previous)
    previous_candidate = "balanced"
    if previous:
        previous_candidate = _candidate(previous[-1], previous[:-1])[0]
    selected = candidate if metrics[-1] or candidate == previous_candidate else "balanced"
    return ModeSignal(candidate, selected, previous_candidate, *metrics)


def _recent_component_usage(history: PublicHistoryProtocol, component: str) -> int:
    total = 0
    observations = history.observations[-10:]
    for observation in observations:
        for rfq in observation.customer_rfqs:
            if component in PRODUCTS[rfq.product_id][0]:
                total += rfq.quantity
    return total // max(1, len(observations) * 6)


def _client_rfq_id(mode: str, component: str, day: int, lead: int) -> str:
    return f"ph-{MODE_CODES[mode]}-{component}-{day:03d}-{lead:02d}"


def _parse_client_rfq_id(value: str) -> tuple[str, str] | None:
    if not value.startswith("ph-"):
        return None
    body, separator, lead = value[3:].rpartition("-")
    intent, day_separator, day = body.rpartition("-")
    mode_code, component_separator, component = intent.partition("-")
    if not separator or not day_separator or not lead.isdigit() or not day.isdigit() or not component_separator:
        return None
    if mode_code not in CODE_MODES or component not in SUPPLIERS:
        return None
    return CODE_MODES[mode_code], component


class PhantAgentSelectorKernel:
    """Approximate dynamic selector over three coherent new policy profiles."""

    strategy_id = "phantagent-adaptive-v1"

    def __init__(self, *, stationary_mode: str | None = None) -> None:
        if stationary_mode is not None and stationary_mode not in PROFILES:
            raise ValueError("unknown PhantAgent-inspired stationary mode")
        self.stationary_mode = stationary_mode
        if stationary_mode is not None:
            self.strategy_id = f"phantagent-stationary-{stationary_mode}-v1"

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
                DecisionExplanation(self.strategy_id, "terminal", "No commitments on the final day.", ("endgame",)),
            )
        signal = select_mode(observation, history)
        mode = self.stationary_mode or signal.selected
        profile = PROFILES[mode]
        orders = _open_orders(observation)
        deliveries = self._deliveries(observation, orders)
        production = self._production(observation, orders, profile, history)
        supplier_orders = self._supplier_orders(observation, profile)
        supplier_rfqs = self._supplier_rfqs(observation, profile, history)
        offers = self._customer_offers(observation, orders, profile)
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
                mode,
                "Select one coherent risk, procurement, sales, and production posture.",
                (
                    f"candidate-{signal.candidate}",
                    f"previous-{signal.previous_candidate}",
                    "immediate-emergency" if signal.emergency else "two-day-hysteresis",
                    "explicitly-approximate",
                ),
                (
                    ("demand_ratio_ppm", signal.demand_ratio_ppm),
                    ("shortage_units", signal.shortage_units),
                    ("factory_load_ppm", signal.factory_load_ppm),
                    ("profitable_rfqs", signal.profitable_rfqs),
                    ("cash_headroom_microcredits", signal.cash_headroom_microcredits),
                    ("mode_code", {"conservative": 0, "balanced": 1, "aggressive": 2}[mode]),
                    ("maximum_position_units", profile.maximum_position),
                    ("target_buffer_units", profile.buffer_units),
                    ("customer_offers", len(offers)),
                    ("supplier_rfqs", len(supplier_rfqs)),
                    ("supplier_orders", len(supplier_orders)),
                ),
            ),
        )

    @staticmethod
    def _supplier_rfqs(observation: DayObservation, profile: ModeProfile, history: PublicHistoryProtocol) -> tuple[SupplierRfqCommand, ...]:
        if observation.day >= 208 or observation.cash_microcredits <= profile.debt_floor:
            return ()
        demand = _component_demand(observation)
        supply = _supply(observation)
        position = _position(observation)
        candidates: list[tuple[int, str, int, int, int]] = []
        for index, component in enumerate(sorted(SUPPLIERS)):
            recent = _recent_component_usage(history, component)
            game_prior = 4
            blended = ((219 - observation.day) * game_prior + observation.day * recent) // 219
            target = min(
                profile.maximum_component_position,
                demand[component] + profile.buffer_units + blended,
            )
            shortage = max(0, target - supply.get(component, 0))
            if shortage == 0:
                continue
            lead = profile.lead_times[(observation.day + index) % len(profile.lead_times)]
            due_day = min(219, observation.day + lead)
            discount_ppm = min(180_000, lead * 4_000)
            cap = BASE_MICROCREDITS[component] * max(500_000, profile.supplier_cap_ppm - discount_ppm) // 1_000_000
            candidates.append((-shortage, component, min(12, shortage), due_day, cap))
        requests: list[SupplierRfqCommand] = []
        for _, component, wanted, due_day, cap in sorted(candidates):
            quantity = min(wanted, profile.maximum_position - position - sum(item.quantity for item in requests))
            if quantity <= 0:
                continue
            requests.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(requests)}",
                    _client_rfq_id(profile.name, component, observation.day, due_day - observation.day),
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
    def _supplier_orders(observation: DayObservation, profile: ModeProfile) -> tuple[SupplierOrderCommand, ...]:
        if observation.day >= 212:
            return ()
        requests = {request.rfq_id: request for request in observation.supplier_rfqs}
        groups: dict[str, list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            request = requests.get(offer.rfq_id)
            parsed = None if request is None else _parse_client_rfq_id(request.client_rfq_id)
            if (
                request is None
                or parsed is None
                or parsed[1] != offer.component_id
                or offer.kind in {"none", "probe"}
                or offer.quantity <= 0
                or offer.unit_price > (request.max_unit_price or 0)
                or offer.unit_price
                > BASE_MICROCREDITS[offer.component_id]
                * profile.supplier_cap_ppm
                // 1_000_000
            ):
                continue
            groups.setdefault(offer.alternative_group_id, []).append(offer)
        outstanding = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        position = _position(observation)
        spend = 0
        units = 0
        selected: list[SupplierOrderCommand] = []
        for group in sorted(groups):
            offer = min(groups[group], key=lambda item: (item.unit_price, item.due_day, -item.quantity, item.offer_id))
            cost = offer.quantity * offer.unit_price
            if (
                position + units + offer.quantity > profile.maximum_position
                or observation.cash_microcredits - outstanding - spend - cost < profile.debt_floor
            ):
                continue
            selected.append(SupplierOrderCommand(f"so-{observation.day}-{len(selected)}", offer.offer_id))
            spend += cost
            units += offer.quantity
        return tuple(selected)

    def _customer_offers(self, observation: DayObservation, orders: tuple[CustomerOrder, ...], profile: ModeProfile) -> tuple[CustomerOfferCommand, ...]:
        if observation.day >= 207:
            return ()
        open_units = sum(order.quantity for order in orders)
        limit = {"conservative": 1, "balanced": 3, "aggressive": 5}[profile.name]
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        if profile.count_near_supply_for_bids:
            for item in observation.pending_inventory:
                if item.item_id in SUPPLIERS:
                    components[item.item_id] = components.get(item.item_id, 0) + item.quantity
            for order in observation.supplier_orders:
                if order.due_day <= observation.day + 2:
                    components[order.component_id] = components.get(
                        order.component_id, 0
                    ) + max(0, order.quantity - order.delivered_quantity)
        candidates: list[tuple[int, str, CustomerRfq, int]] = []
        for rfq in observation.customer_rfqs:
            if not 4 <= rfq.due_day - observation.day <= 12 or open_units + rfq.quantity > profile.maximum_open_customer_units:
                continue
            required = max(0, rfq.quantity - pcs.get(rfq.product_id, 0))
            bom, cycles = PRODUCTS[rfq.product_id]
            if required * cycles > 2_000 or any(components.get(component, 0) < required for component in bom):
                continue
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * (1_000_000 + profile.minimum_margin_ppm) // 1_000_000
            price = min(rfq.unit_reserve, max(floor, rfq.unit_reserve * (850_000 if profile.name == "aggressive" else 920_000) // 1_000_000))
            if price >= floor:
                candidates.append((-(price - nominal) * rfq.quantity, rfq.rfq_id, rfq, price))
        cycles_left = 2_000
        offers: list[CustomerOfferCommand] = []
        for _, _, rfq, price in sorted(candidates):
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
    def _production(observation: DayObservation, orders: tuple[CustomerOrder, ...], profile: ModeProfile, history: PublicHistoryProtocol) -> tuple[ProductionLine, ...]:
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
        if profile.speculative_pc_units and observation.day < 185:
            demand = {product: 0 for product in PRODUCTS}
            for item in history.observations[-5:]:
                for rfq in item.customer_rfqs:
                    demand[rfq.product_id] += rfq.quantity
            product = min(PRODUCTS, key=lambda item: (-demand[item], item))
            bom, unit_cycles = PRODUCTS[product]
            quantity = min(
                profile.speculative_pc_units - pcs.get(product, 0),
                cycles // unit_cycles,
                *[components.get(component, 0) for component in bom],
            )
            if quantity > 0:
                lines.append(ProductionLine(f"pl-{observation.day}-{len(lines)}", len(lines), product, quantity))
        return tuple(lines)

    @staticmethod
    def _deliveries(observation: DayObservation, orders: tuple[CustomerOrder, ...]) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        lines: list[DeliveryLine] = []
        for order in orders:
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            lines.append(DeliveryLine(f"dl-{observation.day}-{len(lines)}", len(lines), order.order_id, order.quantity))
        return tuple(lines)
