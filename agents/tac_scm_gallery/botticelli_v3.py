"""Botticelli-inspired v3: the published scenario planner at scale.

Published mechanisms kept (see the agent's INSPIRATION.md):
nested delivery, expected production and bidding; offers scheduled by expected
quantity; scenario lookahead with a shared first-stage action; marginal utility
of complementary supplier offers; reservation of future factory capacity.

v1's numeric limits were declared 2026 choices (36 open units, 224 position,
32 per component, 10-unit x 8 RFQs capped at base price, 5 offers per day,
bids only with components on hand, forecast / 6, 2 speculative units, a
512-operation budget) and are replaced by ``BotticelliV3Parameters``. Bids are
covered by stock plus confirmed supply at the expected (scenario-weighted)
quantity instead of by on-hand stock only. v1 (`botticelli.py`) is not edited.

``TAC_SCM_GALLERY_BOTTICELLI_V3_PARAMETERS`` may hold a JSON object of overrides.
"""

from __future__ import annotations

from dataclasses import dataclass, fields
import json
import os

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    CustomerOrder,
    CustomerRfq,
    DayObservation,
    ProductionLine,
    SupplierOffer,
    SupplierOrderCommand,
    SupplierRfqCommand,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from . import botticelli as v1
from .botticelli import (
    BotticelliScenarioKernel,
    _component_need,
    _inventory,
    _nominal_product,
    _position,
    _scenarios,
    _supply,
)
from .contracts import PublicHistoryProtocol

PARAMETERS_ENVIRONMENT_VARIABLE = "TAC_SCM_GALLERY_BOTTICELLI_V3_PARAMETERS"
FACTORY_CYCLES = 2_000


@dataclass(frozen=True, slots=True)
class BotticelliV3Parameters:
    forecast_share_divisor: int = 2  # v1: 6
    component_target: int = 1_200  # v1: 32
    maximum_position: int = 16_000  # v1: 224
    maximum_open_units: int = 1_440  # v1: 36
    rfq_quantity_cap: int = 250  # v1: 10
    rfqs_per_supplier: int = 5  # contract limit (v1: 8 in total, one supplier)
    rfq_price_ppm: int = 1_100_000  # v1: base price
    max_offers_per_day: int = 48  # v1: 5
    speculative_days: int = 2  # v1: 2 units
    operation_budget: int = 200_000  # v1: 512
    debt_floor: int = -3_000_000_000_000  # v1: -95bn

    def __post_init__(self) -> None:
        for field in fields(self):
            value = getattr(self, field.name)
            if type(value) is not int or ((value > 0) if field.name == "debt_floor" else (value <= 0)):
                raise ValueError(f"Botticelli v3 parameter {field.name} is out of range")


def parse_parameters(text: str | None) -> BotticelliV3Parameters:
    if text is None or text.strip() == "":
        return BotticelliV3Parameters()
    overrides = json.loads(text)
    if type(overrides) is not dict:
        raise ValueError("Botticelli v3 parameters must be a JSON object")
    unknown = set(overrides) - {field.name for field in fields(BotticelliV3Parameters)}
    if unknown:
        raise ValueError(f"unknown Botticelli v3 parameters: {sorted(unknown)}")
    return BotticelliV3Parameters(**overrides)


def parameters_from_environment() -> BotticelliV3Parameters:
    return parse_parameters(os.environ.get(PARAMETERS_ENVIRONMENT_VARIABLE))


class BotticelliV3Kernel(BotticelliScenarioKernel):
    """v1's scenario planner with declared, factory-scale limits."""

    strategy_id = "botticelli-scenario-v3"

    def __init__(self, *, parameters: BotticelliV3Parameters | None = None) -> None:
        self.parameters = BotticelliV3Parameters() if parameters is None else parameters
        super().__init__(mode="scenario", operation_budget=self.parameters.operation_budget)
        self.strategy_id = "botticelli-scenario-v3"

    def _forecast(self, history: PublicHistoryProtocol) -> dict[str, int]:
        result = {product: 0 for product in PRODUCTS}
        observations = history.observations[-5:]
        for row in observations:
            for rfq in row.customer_rfqs:
                result[rfq.product_id] += rfq.quantity
        divisor = max(1, len(observations) * self.parameters.forecast_share_divisor)
        return {product: quantity // divisor for product, quantity in result.items()}

    def decide(self, observation, features, history):  # type: ignore[no-untyped-def]
        # v1's scenario valuation reads the module forecast but compares one day of
        # demand with ten days of reserved capacity; value demand over the same
        # horizon (published: ten-day lookahead) for this call.
        original = v1._forecast_products
        v1._forecast_products = lambda history: {
            product: units * v1.HORIZON_DAYS for product, units in self._forecast(history).items()
        }
        try:
            return super().decide(observation, features, history)
        finally:
            v1._forecast_products = original

    def _supplier_rfqs(self, observation: DayObservation, history: PublicHistoryProtocol) -> tuple[SupplierRfqCommand, ...]:  # type: ignore[override]
        p = self.parameters
        if observation.day >= 205 or observation.cash_microcredits <= p.debt_floor:
            return ()
        need = _component_need(observation)
        for product, quantity in self._forecast(history).items():
            for component in PRODUCTS[product][0]:
                need[component] += quantity * 3
        supply = _supply(observation)
        position = _position(observation)
        requests: list[SupplierRfqCommand] = []
        per_supplier: dict[str, int] = {}
        for component in sorted(SUPPLIERS, key=lambda item: (-(need[item] - supply.get(item, 0)), item)):
            target = min(p.component_target, need[component] + 5)
            shortage = max(0, target - supply.get(component, 0))
            for supplier in sorted(SUPPLIERS[component]):
                while shortage > 0 and per_supplier.get(supplier, 0) < p.rfqs_per_supplier:
                    quantity = min(p.rfq_quantity_cap, shortage, p.maximum_position - position - sum(r.quantity for r in requests))
                    if quantity <= 0:
                        break
                    lead = 3 + (observation.day + len(requests)) % 10
                    requests.append(
                        SupplierRfqCommand(
                            f"sr3-{observation.day}-{len(requests)}",
                            f"bo-{component}-{observation.day:03d}{len(requests):03d}",
                            supplier,
                            component,
                            quantity,
                            min(219, observation.day + lead),
                            BASE_MICROCREDITS[component] * p.rfq_price_ppm // 1_000_000,
                        )
                    )
                    per_supplier[supplier] = per_supplier.get(supplier, 0) + 1
                    shortage -= quantity
        return tuple(requests)

    def _supplier_orders(self, observation: DayObservation, history: PublicHistoryProtocol, mode: str):  # type: ignore[no-untyped-def,override]
        p = self.parameters
        # v1's offer selection reads these module limits; scale them for this call.
        original = (v1.MAXIMUM_POSITION, v1.DEBT_FLOOR)
        v1.MAXIMUM_POSITION, v1.DEBT_FLOOR = p.maximum_position, p.debt_floor
        try:
            return super()._supplier_orders(observation, history, mode)
        finally:
            v1.MAXIMUM_POSITION, v1.DEBT_FLOOR = original

    def _customer_offers(  # type: ignore[override]
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        history: PublicHistoryProtocol,
        mode: str,
    ) -> tuple[CustomerOfferCommand, ...]:
        p = self.parameters
        day = observation.day
        if day >= 207:
            return ()
        scenarios = _scenarios(mode)
        demand_weight = 1_000_000 if not scenarios else sum(
            s.probability_ppm * s.demand_ppm // 1_000_000 for s in scenarios
        )
        pcs = _inventory(observation.pc_inventory)
        # Cover bids with stock plus confirmed supply, net of open orders.
        cover = _supply(observation)
        for order in orders:
            covered = min(pcs.get(order.product_id, 0), order.quantity)
            pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
            for component in PRODUCTS[order.product_id][0]:
                cover[component] = cover.get(component, 0) - (order.quantity - covered)
        open_units = sum(order.quantity for order in orders)
        candidates: list[tuple[int, str, CustomerRfq, int]] = []
        for rfq in observation.customer_rfqs:
            if not 4 <= rfq.due_day - day <= 12 or open_units + rfq.quantity > p.maximum_open_units:
                continue
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * 1_100_000 // 1_000_000
            price = min(rfq.unit_reserve, max(floor, rfq.unit_reserve * 880_000 // 1_000_000))
            expected = (price - nominal) * rfq.quantity * demand_weight // 1_000_000
            if price >= floor and expected > 0:
                candidates.append((-expected, rfq.rfq_id, rfq, price))
        cycles = FACTORY_CYCLES * 8  # reserved future capacity over the lead window
        result: list[CustomerOfferCommand] = []
        win_ppm = 400_000  # expected quantity: offers win part of the time
        for _, _, rfq, price in sorted(candidates):
            expected_units = -(-rfq.quantity * win_ppm // 1_000_000)
            from_stock = min(max(0, pcs.get(rfq.product_id, 0)), expected_units)
            required = expected_units - from_stock
            bom, unit_cycles = PRODUCTS[rfq.product_id]
            if required * unit_cycles > cycles or any(cover.get(c, 0) < required for c in bom):
                continue
            for c in bom:
                cover[c] -= required
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - from_stock
            cycles -= required * unit_cycles
            result.append(CustomerOfferCommand(f"co3-{day}-{len(result)}", rfq.rfq_id, price))
            open_units += rfq.quantity
            if len(result) == p.max_offers_per_day:
                break
        return tuple(result)

    def _production(  # type: ignore[override]
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        history: PublicHistoryProtocol,
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles_left = FACTORY_CYCLES
        result: list[ProductionLine] = []
        for order in sorted(orders, key=lambda row: (row.due_day, -row.daily_penalty, row.order_id)):
            covered = min(pcs.get(order.product_id, 0), order.quantity)
            pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
            required = order.quantity - covered
            bom, cycles = PRODUCTS[order.product_id]
            build = min([required, cycles_left // cycles] + [components.get(c, 0) for c in bom])
            if build <= 0:
                continue
            for c in bom:
                components[c] -= build
            cycles_left -= build * cycles
            result.append(ProductionLine(f"pl3-{observation.day}-{len(result)}", len(result), order.product_id, build))
        if observation.day < 200 and cycles_left:
            forecast = self._forecast(history)
            for product in sorted(PRODUCTS, key=lambda item: (-forecast[item], item)):
                bom, cycles = PRODUCTS[product]
                target = forecast[product] * self.parameters.speculative_days - max(0, pcs.get(product, 0))
                build = min([target, cycles_left // cycles] + [components.get(c, 0) for c in bom])
                if build <= 0:
                    continue
                for c in bom:
                    components[c] -= build
                cycles_left -= build * cycles
                result.append(ProductionLine(f"pl3-{observation.day}-{len(result)}", len(result), product, build))
        return tuple(result)


__all__ = [
    "PARAMETERS_ENVIRONMENT_VARIABLE",
    "BotticelliV3Kernel",
    "BotticelliV3Parameters",
    "parameters_from_environment",
    "parse_parameters",
]
