"""TacTex-inspired v4 public market state and model features.

One object consumes the agent's own observations day by day and keeps
what TacTex's models read: the demand filter (C1, Bayesian over the game's
(Q, trend) process), a daily price estimate per product (midpoint of the
reported lowest and highest winning prices), the latest market report,
and the supplier model (C5) with its prediction history. The agent and the
offline trainer use the same object, so features at play and at training
are identical.

Feature lists follow D Table 5.1 (C4, 31 features) and D Table 5.4 (C6, 29
features) as closely as the text defines them; our reading of each is in
the names below.
"""

from __future__ import annotations

import numpy as np

from .demand_filter import SEGMENT_BOUNDS, DemandFilter, counts_from_rfq_ids
from tac_scm_sdk.contracts import DayObservation
from tac_scm_sdk.profile import PRODUCTS, SUPPLIERS

from .tactex_v4_models import NOMINAL, PRODUCT_IDS, SupplierModel

SEGMENTS = tuple(SEGMENT_BOUNDS)
C6_LEADS = (5, 10, 15, 20, 30, 40)  # D Table 5.4

C4_FEATURES = (
    "day",
    "seg_demand_now", "seg_demand_10_ago", "seg_demand_20_ago", "seg_demand_pred_10", "seg_trend",
    "total_demand_now", "total_demand_10_ago", "total_demand_20_ago", "total_demand_pred_10", "total_trend",
    "price_now", "price_1_ago", "price_2_ago", "price_5_ago", "price_10_ago", "price_20_ago",
    "change_1", "change_5", "change_10", "change_20",
    "all_price_now", "all_change_10",
    "extrapolate_5", "extrapolate_10",
    "price_vs_segment", "report_low", "report_high",
    "rfq_count_today", "rfq_count_mean_10", "market_report_mean_price",
)
C6_FEATURES = (
    "day", "due_day", "send_lag", "lead",
    "c5_now", "c5_change_20",
    "best_lead_now", "best_lead_later",
    "demand_total", "trend_total",
    "component_share", "is_cpu", "reported_capacity",
    *(f"c5_lead_{n}" for n in C6_LEADS),
    *(f"c5_lead_{n}_change_20" for n in C6_LEADS),
    "pending_units", "inventory_units", "offer_count_today", "price_vs_base_mean",
)
assert len(C4_FEATURES) == 31 and len(C6_FEATURES) == 29


class MarketState:
    def __init__(self, particles: int = 1000) -> None:
        self.demand = DemandFilter(particles)
        self.supplier = SupplierModel()
        self.day = -1
        self.segment_of: dict[str, str] = {}
        self.demand_mean: list[dict[str, float]] = []  # per day: segment -> E[Q]
        self.trend_mean: list[dict[str, float]] = []
        self.prices: dict[str, list[float]] = {p: [] for p in PRODUCT_IDS}  # fraction of nominal
        self.report: dict[str, tuple[float, float] | None] = {p: None for p in PRODUCT_IDS}
        self.awarded_estimate: dict[str, int] = {p: 0 for p in PRODUCT_IDS}
        self.rfq_counts: dict[str, list[int]] = {p: [] for p in PRODUCT_IDS}
        self.market_mean: dict[str, float] = {}
        self.capacity: dict[tuple[str, str], float] = {}
        self.c5_history: dict[tuple[str, str, int], list[float]] = {}  # (supplier, component, lead) -> per day
        self.offers_today: dict[str, int] = {}
        self._forecast: dict[str, np.ndarray] | None = None

    # ------------------------------------------------------------------
    def observe(self, observation: DayObservation) -> None:
        day = observation.day
        self.day = day
        rfq_ids = [r.rfq_id for r in observation.customer_rfqs]
        for rfq in observation.customer_rfqs:
            parts = rfq.rfq_id.split("/")
            if len(parts) == 4:
                self.segment_of[rfq.product_id] = parts[2]
        counts = counts_from_rfq_ids(day, rfq_ids)
        self.demand.observe(counts)
        self.demand_mean.append({s: self.demand.segments[s].mean() for s in SEGMENTS})
        self.trend_mean.append(
            {s: float(np.sum(self.demand.segments[s].weights * self.demand.segments[s].trend)) for s in SEGMENTS}
        )
        self._forecast = None
        per_product = {p: 0 for p in PRODUCT_IDS}
        for rfq in observation.customer_rfqs:
            per_product[rfq.product_id] += 1
        report = {r.product_id: r for r in observation.daily_price_report.products} if observation.daily_price_report else {}
        for p in PRODUCT_IDS:
            self.rfq_counts[p].append(per_product[p])
            row = report.get(p)
            if row is not None and row.minimum is not None and row.maximum is not None:
                low, high = row.minimum / NOMINAL[p], row.maximum / NOMINAL[p]
                self.report[p] = (low, high)
                self.prices[p].append((low + high) / 2)
            else:
                self.report[p] = None
                self.prices[p].append(self.prices[p][-1] if self.prices[p] else float("nan"))
            # Orders awarded yesterday for this product: at most yesterday's RFQs.
            self.awarded_estimate[p] = self.rfq_counts[p][-2] if len(self.rfq_counts[p]) >= 2 else 0
        if observation.market_report is not None:
            for row in observation.market_report.products:
                if row.mean_price is not None:
                    self.market_mean[row.product_id] = row.mean_price / NOMINAL[row.product_id]
            for line in observation.market_report.supplier_lines:
                if line.mean_capacity_nano is not None:
                    self.capacity[(line.supplier_id, line.component_id)] = line.mean_capacity_nano / 1e9
        self.offers_today = {}
        for offer in observation.supplier_offers:
            self.supplier.observe(day, offer.supplier_id, offer.component_id, offer.due_day, offer.unit_price)
            self.offers_today[offer.component_id] = self.offers_today.get(offer.component_id, 0) + 1
        for component, suppliers in SUPPLIERS.items():
            for supplier in suppliers:
                for lead in C6_LEADS:
                    value = self.supplier.price_fraction(supplier, component, day + lead)
                    history = self.c5_history.setdefault((supplier, component, lead), [])
                    history.append(value if value is not None else (history[-1] if history else 1.0))

    # ------------------------------------------------------------------
    def forecast(self) -> dict[str, np.ndarray]:
        if self._forecast is None:
            self._forecast = self.demand.forecast(20)
        return self._forecast

    def segment(self, product: str) -> str:
        return self.segment_of.get(product, "mid")

    def _lag(self, series: list, lag: int) -> float:
        return series[-1 - lag] if len(series) > lag else series[0]

    def _price(self, product: str, lag: int = 0) -> float:
        series = self.prices[product]
        if not series:
            return 1.0
        value = self._lag(series, lag)
        if np.isnan(value):
            known = [v for v in series if not np.isnan(v)]
            return known[0] if known else 1.0
        return value

    def _extrapolate(self, product: str, window: int) -> float:
        series = [v for v in self.prices[product][-window:] if not np.isnan(v)]
        if len(series) < 2:
            return self._price(product)
        x = np.arange(len(series), dtype=float)
        slope, intercept = np.polyfit(x, series, 1)
        return float(intercept + slope * (len(series) - 1 + 10))

    def c4_features(self, product: str) -> np.ndarray:
        s = self.segment(product)
        dm, tm = self.demand_mean, self.trend_mean
        forecast = self.forecast()
        seg_pred = float(forecast[s][9])
        total = lambda row: sum(row.values())  # noqa: E731
        segment_prices = [self._price(p) for p in PRODUCT_IDS if self.segment(p) == s] or [self._price(product)]
        counts = self.rfq_counts[product]
        now = self._price(product)
        all_now = float(np.mean([self._price(p) for p in PRODUCT_IDS]))
        all_10 = float(np.mean([self._price(p, 10) for p in PRODUCT_IDS]))
        report = self.report[product] or (now, now)
        x = [
            self.day / 220,
            dm[-1][s], self._lag(dm, 10)[s], self._lag(dm, 20)[s], seg_pred, tm[-1][s],
            total(dm[-1]), total(self._lag(dm, 10)), total(self._lag(dm, 20)),
            float(sum(forecast[k][9] for k in SEGMENTS)), float(np.mean(list(tm[-1].values()))),
            now, self._price(product, 1), self._price(product, 2), self._price(product, 5),
            self._price(product, 10), self._price(product, 20),
            now - self._price(product, 1), now - self._price(product, 5),
            now - self._price(product, 10), now - self._price(product, 20),
            all_now, all_now - all_10,
            self._extrapolate(product, 5), self._extrapolate(product, 10),
            now - float(np.mean(segment_prices)), report[0], report[1],
            float(counts[-1]) if counts else 0.0, float(np.mean(counts[-10:])) if counts else 0.0,
            self.market_mean.get(product, now),
        ]
        return np.array(x, dtype=float)

    def c6_features(self, supplier: str, component: str, send_lag: int, lead: int, pending: int, inventory: int) -> np.ndarray:
        day = self.day
        due = day + send_lag + lead
        base_now = self.supplier.price_fraction(supplier, component, due)
        c5_now = 1.0 if base_now is None else base_now
        histories = {n: self.c5_history.get((supplier, component, n), [1.0]) for n in C6_LEADS}
        by_lead = [histories[n][-1] for n in C6_LEADS]
        changes = [histories[n][-1] - self._lag(histories[n], 20) for n in C6_LEADS]
        nearest = min(C6_LEADS, key=lambda n: abs(n - lead))
        best_now = float(C6_LEADS[int(np.argmin(by_lead))] == nearest)
        best_later = float(C6_LEADS[int(np.argmin(by_lead))] > nearest)
        dm, tm = self.demand_mean[-1], self.trend_mean[-1]
        users = [p for p in PRODUCT_IDS if component in PRODUCTS[p][0]]
        share = sum(dm[self.segment(p)] for p in users) / max(1e-9, sum(dm[self.segment(p)] for p in PRODUCT_IDS))
        x = [
            day / 220, due / 220, float(send_lag), float(lead),
            c5_now, histories[nearest][-1] - self._lag(histories[nearest], 20),
            best_now, best_later,
            float(sum(dm.values())), float(np.mean(list(tm.values()))),
            share, float(component.startswith("cpu")), self.capacity.get((supplier, component), 550.0) / 550.0,
            *by_lead, *changes,
            float(pending), float(inventory), float(self.offers_today.get(component, 0)),
            float(np.mean(by_lead)),
        ]
        return np.array(x, dtype=float)


__all__ = ["C4_FEATURES", "C6_FEATURES", "C6_LEADS", "MarketState", "SEGMENTS"]
