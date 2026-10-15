"""DeepMaize-inspired v4: v3 with the published learned forecasts.

Kiekintveld et al. 2009 (see `deepmaize_v4_forecast.py`): Pr(win | bid) per
PC type from relative historical nearest neighbours with online affine
calibration, and component prices from linear interpolation of the agent's
own recent quotes plus a learned residual. v3's design (dated reference
trajectory, downstream value, deficit RFQs, value-gated supplier orders,
EDF production) is kept; the forecasts replace v3's heuristics where v3
uses them:

- product value (downstream value of components, make-to-stock gate): v3's
  recent public price becomes the forecast clearing price (Pr(win) = 0.5);
- customer bids: v3's price ``min(reserve, recent price)`` with a 1.1 x
  nominal floor and a constant 35% win rate becomes, per RFQ, the bid that
  maximises Pr(win | bid) x (bid - forecast unit cost) on the relative
  price points (and the reserve), with Pr(win | bid) as the expected win
  rate in v3's component ledger and capacity allocation; candidates are
  ranked by expected profit;
- unit cost: the sum over the bill of materials of the cheapest supplier's
  forecast price (LIPP + residual) for an RFQ sent today, due two days
  before the order.

Without a fitted forecast for a product (no price reports yet, or no model
directory), v4 uses v3's rules for it, so an untrained v4 plays as v3.

The model directory holds ``pc-database.npz`` and
``component-residual.json``, fitted offline from game records
(agents/MODELS.md); the entry point passes it explicitly.
"""

from __future__ import annotations

import os
from pathlib import Path

import numpy as np

from tac_scm_sdk.contracts import CustomerOfferCommand, CustomerOrder, DayObservation
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from .contracts import FeatureVector, KernelDecision, PublicHistoryProtocol
from .deepmaize import _inventory, _nominal_product
from .deepmaize_v3 import FACTORY_CYCLES, PPM, DeepMaizeV3Kernel, DeepMaizeV3Parameters
from .deepmaize_v4_forecast import (
    POINTS,
    AffineCalibration,
    ComponentQuotes,
    PcMarketState,
    PcPriceModel,
    ResidualTrees,
    relative_index,
)

MODELS_ENVIRONMENT_VARIABLE = "TAC_SCM_GALLERY_DEEPMAIZE_V4_MODELS"


def models_from_environment() -> Path | None:
    value = os.environ.get(MODELS_ENVIRONMENT_VARIABLE)
    return Path(value) if value else None


class DeepMaizeV4Kernel(DeepMaizeV3Kernel):
    strategy_id = "deepmaize-feedback-v4"

    def __init__(self, *, parameters: DeepMaizeV3Parameters | None = None, models: Path | None = None) -> None:
        super().__init__(parameters=parameters)
        self.market = PcMarketState()
        self.quotes = ComponentQuotes()
        self.calibration = AffineCalibration()
        self.pc_model = PcPriceModel(models / "pc-database.npz") if models is not None else None
        self.residual = ResidualTrees(models / "component-residual.json") if models is not None else None
        self._curves: dict[str, tuple[np.ndarray, np.ndarray, np.ndarray]] = {}  # grid, base, calibrated
        self._costs: dict[tuple[str, int], float] = {}
        self.stats = {"forecast_bids": 0, "fallback_bids": 0}

    def decide(
        self, observation: DayObservation, features: FeatureVector, history: PublicHistoryProtocol
    ) -> KernelDecision:
        self.market.observe(observation)
        self.quotes.observe(observation)
        self.calibration.observe(observation.day, {order.rfq_id for order in observation.new_customer_orders})
        self._curves = {}
        self._costs = {}
        if self.pc_model is not None:
            for product in PRODUCTS:
                row = self.market.features(product)
                grid = self.market.grid(product)
                if row is None or grid is None:
                    continue
                base = self.pc_model.predict(product, row)
                if base is not None:
                    self._curves[product] = (grid, base, self.calibration.apply(base))
        return super().decide(observation, features, history)

    # -- forecasts in v3's places ---------------------------------------------

    def _product_values(self, observation: DayObservation) -> dict[str, int]:
        values = super()._product_values(observation)
        for product, (grid, _base, curve) in self._curves.items():
            below = np.nonzero(curve < 0.5)[0]
            index = int(below[0]) if len(below) else POINTS - 1
            values[product] = int(grid[index])
        return values

    def _component_price(self, day: int, component: str, due: int) -> float:
        key = (component, due)
        if key not in self._costs:
            best = float("inf")
            for supplier in SUPPLIERS[component]:
                x = self.quotes.features(self.market, day, supplier, component, 0, due)
                lipp = x[4]
                residual = self.residual.predict(component, x) if self.residual is not None else 0.0
                best = min(best, max(0.05, lipp + residual))
            self._costs[key] = best * BASE_MICROCREDITS[component]
        return self._costs[key]

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
        # (expected profit, rfq id, rfq, price, win probability, base P, relative index or None)
        candidates = []
        for rfq in observation.customer_rfqs:
            lead = rfq.due_day - day
            if not 3 <= lead <= 12:
                continue
            nominal = _nominal_product(rfq.product_id)
            forecast = self._curves.get(rfq.product_id)
            if forecast is None:
                floor = nominal * p.min_margin_ppm // PPM
                price = min(rfq.unit_reserve, values[rfq.product_id] * p.price_markup_ppm // PPM)
                if price < floor:
                    continue
                win = p.sales_win_rate_ppm / PPM
                candidates.append((win * (price - nominal) * rfq.quantity, rfq.rfq_id, rfq, price, win, None, None))
                continue
            grid, base, curve = forecast
            due = max(day + 2, rfq.due_day - 2)
            cost = sum(self._component_price(day, c, due) for c in PRODUCTS[rfq.product_id][0])
            prices = np.append(grid[grid <= rfq.unit_reserve], float(rfq.unit_reserve))
            wins = np.interp(prices, grid, curve, left=float(curve[0]), right=0.0)
            if prices[-1] > grid[-1]:
                wins[-1] = 0.0
            profit = wins * (prices - cost)
            best = int(np.argmax(profit))
            if profit[best] <= 0:
                continue
            price = int(prices[best])
            index = relative_index(price, grid)
            base_win = float(np.interp(price, grid, base, left=float(base[0]), right=0.0))
            candidates.append((float(profit[best]) * rfq.quantity, rfq.rfq_id, rfq, price, float(wins[best]), base_win, index))
        # v3's component ledger and capacity, at the expected win rate.
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
        for _profit, _id, rfq, price, win, base_win, index in sorted(candidates, key=lambda row: (-row[0], row[1])):
            expected_units = -(-int(rfq.quantity * win * PPM) // PPM)
            from_stock = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            needed = max(0, expected_units - from_stock)
            bom = PRODUCTS[rfq.product_id][0]
            if needed and any(available(c, rfq.due_day - 2) < needed for c in bom):
                continue
            cycles = int(rfq.quantity * PRODUCTS[rfq.product_id][1] * win)
            window = range(0, max(1, rfq.due_day - 1 - day))
            load = sum(committed[i] for i in window)
            if load + cycles > capacity * len(window):
                continue
            offers.append(CustomerOfferCommand(f"dm4-c-{day}-{len(offers)}", rfq.rfq_id, price))
            if base_win is not None and index is not None:
                self.calibration.record_bid(rfq.rfq_id, day, base_win, index)
                self.stats["forecast_bids"] += 1
            else:
                self.stats["fallback_bids"] += 1
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - min(from_stock, expected_units)
            for c in bom:
                allocated[c] += needed
            committed[max(0, min(12, rfq.due_day - 1 - day))] += cycles
            if len(offers) == p.max_offers_per_day:
                break
        return tuple(offers)


__all__ = ["MODELS_ENVIRONMENT_VARIABLE", "DeepMaizeV4Kernel", "models_from_environment"]
