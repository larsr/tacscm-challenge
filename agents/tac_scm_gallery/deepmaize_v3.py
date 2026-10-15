"""DeepMaize-inspired v3: the published value-feedback design at scale.

Published mechanisms kept (see the agent's INSPIRATION.md): a dated reference inventory
trajectory built from outstanding obligations, expected future utilisation and
a baseline buffer, minus on-hand and anticipated supply; RFQs that close dated
deficits in priority order; downstream value propagated from product revenue to
components; supplier-offer acceptance by downstream value minus cost; separate
feedback-coordinated procurement, production and sales. The historical day-zero
preemption is excluded (a 2003 rule).

v1's numeric limits were declared 2026 choices (20 open units, 256 position,
32 units per day, one obligation at a time, 8-14 day leads, -100bn floor) and
are replaced by ``DeepMaizeV3Parameters`` sized to the game's 2,000 daily
cycles. v1 (`deepmaize.py`) is not edited. Customer prices follow v1's declared public-price heuristic (recent
market prices; the learned price distributions of the 2009 paper remain
deferred), never above the reserve.

``TAC_SCM_GALLERY_DEEPMAIZE_V3_PARAMETERS`` may hold a JSON object of overrides.
"""

from __future__ import annotations

from dataclasses import dataclass, fields
import json
import os

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    CustomerOrder,
    DayActionSet,
    DayObservation,
    DeliveryLine,
    ProductionLine,
    SupplierOrderCommand,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol
from .deepmaize import _inventory, _nominal_product, _open_orders

PPM = 1_000_000
FACTORY_CYCLES = 2_000
PARAMETERS_ENVIRONMENT_VARIABLE = "TAC_SCM_GALLERY_DEEPMAIZE_V3_PARAMETERS"


@dataclass(frozen=True, slots=True)
class DeepMaizeV3Parameters:
    utilization_ppm: int = 900_000  # expected factory utilisation behind the targets
    buffer_days: int = 3  # baseline buffer in days of expected usage
    rfq_lead_days: int = 3  # near dated deficit
    far_lead_days: int = 8  # far dated deficit
    rfq_quantity_cap: int = 250
    rfqs_per_supplier: int = 5  # contract: at most five positive RFQs per supplier
    supplier_value_ppm: int = 950_000  # pay at most this share of downstream value
    order_slack_units: int = 200
    sales_win_rate_ppm: int = 350_000  # expected acceptance for capacity allocation
    price_markup_ppm: int = 1_000_000  # bid = recent market price x markup, <= reserve
    min_margin_ppm: int = 1_100_000  # floor: nominal x this
    max_offers_per_day: int = 64
    stock_buffer_days: int = 2  # make-to-stock PCs in days of expected sales
    last_offer_day: int = 205
    last_procurement_day: int = 210
    debt_floor: int = -3_000_000_000_000

    def __post_init__(self) -> None:
        for field in fields(self):
            value = getattr(self, field.name)
            if type(value) is not int:
                raise ValueError(f"DeepMaize v3 parameter {field.name} must be an integer")
            if (value > 0) if field.name == "debt_floor" else (value < 0):
                raise ValueError(f"DeepMaize v3 parameter {field.name} is out of range")


def parse_parameters(text: str | None) -> DeepMaizeV3Parameters:
    if text is None or text.strip() == "":
        return DeepMaizeV3Parameters()
    overrides = json.loads(text)
    if type(overrides) is not dict:
        raise ValueError("DeepMaize v3 parameters must be a JSON object")
    unknown = set(overrides) - {field.name for field in fields(DeepMaizeV3Parameters)}
    if unknown:
        raise ValueError(f"unknown DeepMaize v3 parameters: {sorted(unknown)}")
    return DeepMaizeV3Parameters(**overrides)


def parameters_from_environment() -> DeepMaizeV3Parameters:
    return parse_parameters(os.environ.get(PARAMETERS_ENVIRONMENT_VARIABLE))


def _market_price(observation: DayObservation, product_id: str) -> int | None:
    """Recent public clearing price: market-report mean, else daily range midpoint."""
    if observation.market_report is not None:
        for row in observation.market_report.products:
            if row.product_id == product_id and row.mean_price is not None:
                return row.mean_price
    if observation.daily_price_report is not None:
        for row in observation.daily_price_report.products:
            if row.product_id == product_id and row.minimum is not None and row.maximum is not None:
                return (row.minimum + row.maximum) // 2
    return None


class DeepMaizeV3Kernel:
    """Dated reference-trajectory control with downstream value feedback."""

    strategy_id = "deepmaize-feedback-v3"

    def __init__(self, *, parameters: DeepMaizeV3Parameters | None = None) -> None:
        self.parameters = DeepMaizeV3Parameters() if parameters is None else parameters
        self._demand_history: list[dict[str, int]] = []

    # -- expected utilisation and downstream value --------------------------

    def _product_mix(self, observation: DayObservation) -> dict[str, int]:
        """Expected units per day per product at the utilisation target."""
        today = {product: 0 for product in PRODUCTS}
        for rfq in observation.customer_rfqs:
            today[rfq.product_id] += rfq.quantity
        self._demand_history.append(today)
        window = self._demand_history[-5:]
        demand = {product: sum(row[product] for row in window) for product in PRODUCTS}
        total_cycles = sum(demand[p] * PRODUCTS[p][1] for p in PRODUCTS)
        budget = FACTORY_CYCLES * self.parameters.utilization_ppm // PPM
        if total_cycles == 0:
            return {product: 0 for product in PRODUCTS}
        return {p: budget * demand[p] // total_cycles for p in PRODUCTS}

    def _product_values(self, observation: DayObservation) -> dict[str, int]:
        values = {}
        for product in PRODUCTS:
            market = _market_price(observation, product)
            values[product] = market if market is not None else _nominal_product(product) * 6 // 5
        return values

    def _component_values(self, product_values: dict[str, int]) -> dict[str, int]:
        values = dict(BASE_MICROCREDITS)
        for product, value in product_values.items():
            nominal = _nominal_product(product)
            for component in PRODUCTS[product][0]:
                values[component] = max(values[component], value * BASE_MICROCREDITS[component] // nominal)
        return values

    # -- dated reference trajectory ------------------------------------------

    def _deficits(
        self, observation: DayObservation, orders: tuple[CustomerOrder, ...], mix: dict[str, int]
    ) -> dict[str, tuple[int, int]]:
        """Per component: (near deficit, far deficit) against the reference trajectory."""
        p = self.parameters
        day = observation.day
        pcs = _inventory(observation.pc_inventory)
        need_by: dict[str, list[tuple[int, int]]] = {c: [] for c in SUPPLIERS}
        for order in orders:
            covered = min(pcs.get(order.product_id, 0), order.quantity)
            pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
            for component in PRODUCTS[order.product_id][0]:
                need_by[component].append((max(day + 1, order.due_day - 1), order.quantity - covered))
        usage = {c: 0 for c in SUPPLIERS}
        for product, units in mix.items():
            for component in PRODUCTS[product][0]:
                usage[component] += units
        stock = _inventory(observation.component_inventory)
        arrivals: dict[str, list[tuple[int, int]]] = {c: [] for c in SUPPLIERS}
        for item in observation.pending_inventory:
            if item.item_id in SUPPLIERS:
                arrivals[item.item_id].append((day + 1, item.quantity))
        for order in observation.supplier_orders:
            remaining = max(0, order.quantity - order.delivered_quantity)
            if remaining:
                arrivals[order.component_id].append((max(day + 1, order.due_day), remaining))
        result = {}
        for component in SUPPLIERS:
            points = []
            for horizon in (p.rfq_lead_days, p.far_lead_days):
                until = day + horizon
                obligations = sum(q for due, q in need_by[component] if due <= until + p.buffer_days)
                target = obligations + usage[component] * (horizon + p.buffer_days)
                supply = stock.get(component, 0) + sum(q for due, q in arrivals[component] if due <= until)
                points.append(max(0, target - supply))
            near, far = points
            result[component] = (near, max(0, far - near))
        return result

    # -- decisions -----------------------------------------------------------

    def decide(
        self, observation: DayObservation, features: FeatureVector, history: PublicHistoryProtocol
    ) -> KernelDecision:
        del features, history
        if observation.day == 219:
            return KernelDecision(
                empty_action(observation),
                DecisionExplanation(self.strategy_id, "terminal", "No commitments on the final day.", ("endgame",)),
            )
        orders = _open_orders(observation)
        mix = self._product_mix(observation)
        product_values = self._product_values(observation)
        component_values = self._component_values(product_values)
        deficits = self._deficits(observation, orders, mix)
        deliveries = self._deliveries(observation, orders)
        production = self._production(observation, orders, mix, product_values)
        supplier_orders = self._supplier_orders(observation, deficits, component_values)
        requests = self._supplier_rfqs(observation, deficits, component_values)
        offers = self._offers(observation, orders, mix, product_values)
        action = DayActionSet(
            observation.agent_id, observation.observation_id, observation.day, 1,
            offers, requests, supplier_orders, production, deliveries,
        )
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "value-feedback",
                "Close dated reference-trajectory deficits with downstream-value feedback at factory scale.",
                ("reference-trajectory", "downstream-value", "expected-utilisation"),
                (
                    ("near_deficit_units", sum(v[0] for v in deficits.values())),
                    ("far_deficit_units", sum(v[1] for v in deficits.values())),
                    ("customer_offers", len(offers)),
                    ("supplier_rfqs", len(requests)),
                    ("supplier_orders", len(supplier_orders)),
                ),
            ),
        )

    def _supplier_rfqs(
        self, observation: DayObservation, deficits: dict[str, tuple[int, int]], values: dict[str, int]
    ) -> tuple[SupplierRfqCommand, ...]:
        p = self.parameters
        day = observation.day
        if day > p.last_procurement_day:
            return ()
        requests: list[SupplierRfqCommand] = []
        per_supplier: dict[str, int] = {}
        ordered = sorted(SUPPLIERS, key=lambda c: (-deficits[c][0], -deficits[c][1], c))
        for component in ordered:
            for lead, deficit in ((p.rfq_lead_days, deficits[component][0]), (p.far_lead_days, deficits[component][1])):
                due = min(219, day + lead)
                if deficit <= 0 or due < day + 2:
                    continue
                suppliers = sorted(SUPPLIERS[component])
                share = -(-deficit // len(suppliers))
                for supplier in suppliers:
                    remaining = min(share, deficit)
                    while remaining > 0 and per_supplier.get(supplier, 0) < p.rfqs_per_supplier:
                        quantity = min(remaining, p.rfq_quantity_cap)
                        requests.append(
                            SupplierRfqCommand(
                                f"dm3-r-{day}-{len(requests)}",
                                f"dm3-rfq-{day}-{len(requests)}",
                                supplier,
                                component,
                                quantity,
                                due,
                                max(values[component] * p.supplier_value_ppm // PPM, BASE_MICROCREDITS[component] * 2),
                            )
                        )
                        per_supplier[supplier] = per_supplier.get(supplier, 0) + 1
                        remaining -= quantity
                        deficit -= quantity
        return tuple(requests)

    def _supplier_orders(
        self, observation: DayObservation, deficits: dict[str, tuple[int, int]], values: dict[str, int]
    ) -> tuple[SupplierOrderCommand, ...]:
        p = self.parameters
        if observation.day > p.last_procurement_day:
            return ()
        groups: dict[str, list] = {}
        for offer in observation.supplier_offers:
            if offer.kind in {"none", "probe"} or offer.quantity <= 0 or offer.due_day < observation.day:
                continue
            groups.setdefault(offer.alternative_group_id, []).append(offer)
        best = []
        for rows in groups.values():
            offer = min(rows, key=lambda row: (row.due_day, row.unit_price, -row.quantity, row.offer_id))
            surplus = values[offer.component_id] * p.supplier_value_ppm // PPM - offer.unit_price
            if surplus > 0:
                best.append((-surplus * offer.quantity, offer.offer_id, offer))
        remaining = {c: deficits[c][0] + deficits[c][1] + p.order_slack_units for c in SUPPLIERS}
        outstanding = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price for order in observation.supplier_orders
        )
        cash = observation.cash_microcredits - outstanding
        commands: list[SupplierOrderCommand] = []
        for _value, _id, offer in sorted(best):
            cost = offer.quantity * offer.unit_price
            if offer.quantity > remaining[offer.component_id] or cash - cost < p.debt_floor:
                continue
            commands.append(SupplierOrderCommand(f"dm3-o-{observation.day}-{len(commands)}", offer.offer_id))
            remaining[offer.component_id] -= offer.quantity
            cash -= cost
        return tuple(commands)

    def _offers(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        mix: dict[str, int],
        values: dict[str, int],
    ) -> tuple[CustomerOfferCommand, ...]:
        p = self.parameters
        day = observation.day
        if day > p.last_offer_day or observation.cash_microcredits <= p.debt_floor:
            return ()
        committed = [0] * 13
        for order in orders:
            slot = max(0, min(12, order.due_day - 1 - day))
            committed[slot] += order.quantity * PRODUCTS[order.product_id][1]
        capacity = FACTORY_CYCLES * p.utilization_ppm // PPM
        candidates = []
        for rfq in observation.customer_rfqs:
            lead = rfq.due_day - day
            if not 3 <= lead <= 12:
                continue
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * p.min_margin_ppm // PPM
            price = min(rfq.unit_reserve, values[rfq.product_id] * p.price_markup_ppm // PPM)
            if price < floor:
                continue
            candidates.append(((price - nominal) * rfq.quantity, rfq.rfq_id, rfq, price))
        # Component ledger: stock plus confirmed arrivals, minus open-order needs,
        # so sales follow the same inventory trajectory that drives procurement.
        pcs = _inventory(observation.pc_inventory)
        ledger: dict[str, list[tuple[int, int]]] = {c: [(day, q)] for c, q in _inventory(observation.component_inventory).items()}
        for item in observation.pending_inventory:
            if item.item_id in SUPPLIERS:
                ledger.setdefault(item.item_id, []).append((day + 1, item.quantity))
        for order in observation.supplier_orders:
            remaining = max(0, order.quantity - order.delivered_quantity)
            if remaining:
                ledger.setdefault(order.component_id, []).append((max(day + 1, order.due_day), remaining))
        allocated = {c: 0 for c in SUPPLIERS}
        for order in orders:
            covered = min(pcs.get(order.product_id, 0), order.quantity)
            pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
            for component in PRODUCTS[order.product_id][0]:
                allocated[component] += order.quantity - covered

        def available(component: str, by_day: int) -> int:
            return sum(q for due, q in ledger.get(component, ()) if due <= by_day) - allocated[component]

        offers: list[CustomerOfferCommand] = []
        for _margin, _id, rfq, price in sorted(candidates, key=lambda row: (-row[0], row[1])):
            expected_units = -(-rfq.quantity * p.sales_win_rate_ppm // PPM)
            from_stock = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            needed = max(0, expected_units - from_stock)
            bom = PRODUCTS[rfq.product_id][0]
            if needed and any(available(c, rfq.due_day - 2) < needed for c in bom):
                continue
            cycles = rfq.quantity * PRODUCTS[rfq.product_id][1] * p.sales_win_rate_ppm // PPM
            window = range(0, max(1, rfq.due_day - 1 - day))
            load = sum(committed[i] for i in window)
            if load + cycles > capacity * len(window):
                continue
            offers.append(CustomerOfferCommand(f"dm3-c-{day}-{len(offers)}", rfq.rfq_id, price))
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - min(from_stock, expected_units)
            for c in bom:
                allocated[c] += needed
            committed[max(0, min(12, rfq.due_day - 1 - day))] += cycles
            if len(offers) == p.max_offers_per_day:
                break
        return tuple(offers)

    def _production(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        mix: dict[str, int],
        values: dict[str, int],
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles = FACTORY_CYCLES
        planned: dict[str, int] = {}

        def build(product: str, quantity: int) -> int:
            nonlocal cycles
            bom, unit_cycles = PRODUCTS[product]
            feasible = min([quantity, cycles // unit_cycles] + [components.get(c, 0) for c in bom])
            if feasible <= 0:
                return 0
            for c in bom:
                components[c] -= feasible
            cycles -= feasible * unit_cycles
            planned[product] = planned.get(product, 0) + feasible
            return feasible

        for order in orders:  # earliest due first
            available = pcs.get(order.product_id, 0)
            covered = min(available, order.quantity)
            pcs[order.product_id] = available - covered
            if order.quantity > covered:
                build(order.product_id, order.quantity - covered)
        if observation.day < self.parameters.last_offer_day:
            ranked = sorted(PRODUCTS, key=lambda prod: (-(values[prod] - _nominal_product(prod)), prod))
            for product in ranked:
                target = mix[product] * self.parameters.stock_buffer_days
                gap = target - pcs.get(product, 0) - planned.get(product, 0)
                if gap > 0 and values[product] > _nominal_product(product):
                    build(product, gap)
        return tuple(
            ProductionLine(f"dm3-p-{observation.day}-{index}", index, product, quantity)
            for index, (product, quantity) in enumerate(sorted(planned.items()))
        )

    @staticmethod
    def _deliveries(observation: DayObservation, orders: tuple[CustomerOrder, ...]) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        lines: list[DeliveryLine] = []
        for order in orders:
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            lines.append(DeliveryLine(f"dm3-d-{observation.day}-{len(lines)}", len(lines), order.order_id, order.quantity))
        return tuple(lines)


__all__ = [
    "PARAMETERS_ENVIRONMENT_VARIABLE",
    "DeepMaizeV3Kernel",
    "DeepMaizeV3Parameters",
    "parameters_from_environment",
    "parse_parameters",
]
