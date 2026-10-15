"""DeepMaize-inspired v4 price forecasts, shared by the agent and its trainer.

Published design (Kiekintveld et al. 2009, "Forecasting market prices in a
supply chain game", sections 4 and 6):

PC market (section 4):
- the state of a PC type on a day is a feature vector: average reported high
  and low selling price over the previous five days, the estimated mean
  customer demand Q (Bayesian demand model), the day, and the linear trend of
  selling prices over the previous ten days; features are normalised by
  their training standard deviation, weighted equally and each bounded;
- the relative representation: Pr(win | bid) at 100 price points, 20 below
  the five-day average low, 60 between it and the average high, 20 above;
  historical distributions use the lowest competing price per request with
  one randomly chosen agent's bid removed, smoothed over requests on days
  d - 10 .. d + 10;
- k = 18 nearest historical states, at most one per game, averaged point by
  point, then made monotone;
- online learning: an affine correction a * P + c chosen by brute force over
  a bounded grid to minimise the log score of the agent's recent bids won
  and lost, applied only within the price range where those bids were made.

Component market (section 6): the linear interpolation price predictor
(LIPP) over the agent's own quotes of the last day per supplier line, plus a
learned residual (RCPP) from a depth-5 reduced-error-pruned regression tree
per component type over market indicators (Table 3).

The concrete point placement outside [low, high], the feature bound, the
calibration grid and the dropped attributes are 2026 choices (see the
agent's INSPIRATION.md).
"""

from __future__ import annotations

import json
import math
from pathlib import Path

import numpy as np

from .demand_filter import SEGMENT_BOUNDS, DemandFilter, counts_from_rfq_ids
from tac_scm_sdk.contracts import DayObservation
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

POINTS = 100
K_NEIGHBOURS = 18
SMOOTH_DAYS = 10
PC_FEATURES = ("avg_low_5", "avg_high_5", "demand_q", "day", "trend_10")
SEGMENTS = tuple(SEGMENT_BOUNDS)
PRODUCT_IDS = tuple(sorted(PRODUCTS))
NOMINAL = {p: sum(BASE_MICROCREDITS[c] for c in PRODUCTS[p][0]) for p in PRODUCT_IDS}
COMPONENT_TYPES = ("cpu", "board", "memory", "disk")
SEND_LAGS = (0, 1, 2, 3, 5, 10, 20)
RESIDUAL_FEATURES = (
    "day", "send_lag", "duration", "due_offset", "lipp", "nearest_point", "days_since_data",
    "market_capacity", "demand_5", "demand_20", "demand_40", "mean_demand", "low_demand", "mid_demand", "high_demand",
)


def component_type(component: str) -> str:
    for kind in COMPONENT_TYPES:
        if component.startswith(kind):
            return kind
    return component.split("-")[0]


# -- relative price representation ---------------------------------------

def relative_grid(low: float, high: float) -> np.ndarray:
    """100 prices: 20 from 0.9 low up to low, 60 from low up to high,
    20 from high to 1.1 high."""
    high = max(high, low * (1 + 1e-6))
    below = low * (0.9 + 0.1 * np.arange(20) / 20)
    middle = low + (high - low) * np.arange(60) / 60
    above = high * (1 + 0.1 * np.arange(20) / 19)
    return np.concatenate([below, middle, above])


def relative_index(price: float, grid: np.ndarray) -> float:
    """Continuous position of a price on a grid; -1 below, 100 above."""
    if price < grid[0]:
        return -1.0
    if price > grid[-1]:
        return float(POINTS)
    return float(np.interp(price, grid, np.arange(POINTS)))


def win_curve(thresholds: np.ndarray) -> np.ndarray:
    """Pr(win) at each point: fraction of requests whose (relative) lowest
    competing price lies above the point."""
    if len(thresholds) == 0:
        return np.full(POINTS, np.nan)
    points = np.arange(POINTS)
    return (thresholds[None, :] > points[:, None]).mean(axis=1)


def monotone(curve: np.ndarray) -> np.ndarray:
    """Pr(win) must not rise with price."""
    return np.minimum.accumulate(curve)


# -- public PC market state -------------------------------------------------

class PcMarketState:
    """Consumes one agent's observations day by day; features at play and at
    training are computed by this object."""

    def __init__(self, particles: int = 1000) -> None:
        self.demand = DemandFilter(particles)
        self.segment_of: dict[str, str] = {}
        self.day = -1
        self.lows: dict[str, list[float | None]] = {p: [] for p in PRODUCT_IDS}
        self.highs: dict[str, list[float | None]] = {p: [] for p in PRODUCT_IDS}
        self.q_mean: dict[str, float] = {s: float("nan") for s in SEGMENTS}
        self.demand_history: list[dict[str, int]] = []  # per day: component -> units requested

    def observe(self, observation: DayObservation) -> None:
        self.day = observation.day
        for rfq in observation.customer_rfqs:
            parts = rfq.rfq_id.split("/")
            if len(parts) == 4:
                self.segment_of[rfq.product_id] = parts[2]
        self.demand.observe(counts_from_rfq_ids(observation.day, [r.rfq_id for r in observation.customer_rfqs]))
        self.q_mean = {s: float(self.demand.segments[s].mean()) for s in SEGMENTS}
        report = {r.product_id: r for r in observation.daily_price_report.products} if observation.daily_price_report else {}
        for p in PRODUCT_IDS:
            row = report.get(p)
            ok = row is not None and row.minimum is not None and row.maximum is not None
            self.lows[p].append(float(row.minimum) if ok else None)  # type: ignore[union-attr]
            self.highs[p].append(float(row.maximum) if ok else None)  # type: ignore[union-attr]
        units = {c: 0 for c in SUPPLIERS}
        for rfq in observation.customer_rfqs:
            for c in PRODUCTS[rfq.product_id][0]:
                units[c] += rfq.quantity
        self.demand_history.append(units)

    def band(self, product: str) -> tuple[float, float] | None:
        """Five-day average reported low and high."""
        lows = [v for v in self.lows[product][-5:] if v is not None]
        highs = [v for v in self.highs[product][-5:] if v is not None]
        if not lows or not highs:
            return None
        return sum(lows) / len(lows), sum(highs) / len(highs)

    def features(self, product: str) -> np.ndarray | None:
        band = self.band(product)
        if band is None:
            return None
        nominal = NOMINAL[product]
        mids = [
            ((lo + hi) / 2 / nominal, i)
            for i, (lo, hi) in enumerate(zip(self.lows[product][-10:], self.highs[product][-10:]))
            if lo is not None and hi is not None
        ]
        trend = 0.0
        if len(mids) >= 2:
            xs = np.array([i for _, i in mids], dtype=float)
            ys = np.array([m for m, _ in mids])
            trend = float(np.polyfit(xs, ys, 1)[0])
        q = self.q_mean.get(self.segment_of.get(product, "mid"), float("nan"))
        return np.array([band[0] / nominal, band[1] / nominal, q, float(self.day), trend])

    def grid(self, product: str) -> np.ndarray | None:
        band = self.band(product)
        return None if band is None else relative_grid(*band)


# -- kNN distributions -------------------------------------------------------

class PcPriceModel:
    """Historical database per product: state features, game index and the
    100-point Pr(win) curve (uint8, /255)."""

    def __init__(self, path: Path) -> None:
        data = np.load(path)
        self.scale = data["scale"]
        self.bound = float(data["bound"])
        self.tables: dict[str, tuple[np.ndarray, np.ndarray, np.ndarray]] = {}
        for p in PRODUCT_IDS:
            features = data[f"{p}_features"] / self.scale
            games = data[f"{p}_games"]
            curves = data[f"{p}_curves"]
            self.tables[p] = (features, games, curves)

    def predict(self, product: str, features: np.ndarray, exclude_game: int | None = None) -> np.ndarray | None:
        table = self.tables.get(product)
        if table is None or not np.all(np.isfinite(features)):
            return None
        stored, games, curves = table
        delta = np.minimum(np.abs(stored - features / self.scale) / self.bound, 1.0)
        distance = np.sqrt((delta ** 2).sum(axis=1))
        if exclude_game is not None:
            distance = np.where(games == exclude_game, np.inf, distance)
        # At most one state per historical game: its nearest.
        order = np.lexsort((distance, games))
        first = np.ones(len(order), dtype=bool)
        first[1:] = games[order][1:] != games[order][:-1]
        best = order[first]
        best = best[np.isfinite(distance[best])]
        if len(best) == 0:
            return None
        chosen = best[np.argsort(distance[best], kind="stable")[:K_NEIGHBOURS]]
        curve = curves[chosen].astype(float).mean(axis=0) / 255.0
        return monotone(curve)


class AffineCalibration:
    """a * P + c over a bounded grid, scored by the log rule on the agent's
    recent resolved bids; applied only inside their relative price range."""

    A_GRID = tuple(round(0.5 + 0.1 * i, 2) for i in range(11))  # 0.5 .. 1.5
    C_GRID = tuple(round(-0.3 + 0.05 * i, 2) for i in range(13))  # -0.3 .. 0.3

    def __init__(self, window_days: int = 5) -> None:
        self.window_days = window_days
        self.pending: dict[str, tuple[int, float, float]] = {}  # rfq id -> day, base P, relative index
        self.resolved: list[tuple[int, float, float, int]] = []  # day, base P, index, won
        self.a, self.c = 1.0, 0.0
        self.span: tuple[float, float] | None = None

    def record_bid(self, rfq_id: str, day: int, base_probability: float, index: float) -> None:
        self.pending[rfq_id] = (day, base_probability, index)

    def observe(self, day: int, won_rfq_ids: set[str]) -> None:
        for rfq_id, (bid_day, probability, index) in list(self.pending.items()):
            if bid_day < day:
                self.resolved.append((bid_day, probability, index, int(rfq_id in won_rfq_ids)))
                del self.pending[rfq_id]
        self.resolved = [row for row in self.resolved if row[0] >= day - self.window_days]
        if not self.resolved:
            self.a, self.c, self.span = 1.0, 0.0, None
            return
        probabilities = np.array([row[1] for row in self.resolved])
        outcomes = np.array([row[3] for row in self.resolved])
        best = (math.inf, 1.0, 0.0)
        for a in self.A_GRID:
            for c in self.C_GRID:
                adjusted = np.clip(a * probabilities + c, 0.001, 0.999)
                score = -np.log(1 - np.abs(adjusted - outcomes)).sum()
                if score < best[0] - 1e-12:
                    best = (score, a, c)
        _, self.a, self.c = best
        indices = [row[2] for row in self.resolved]
        self.span = (min(indices), max(indices))

    def apply(self, curve: np.ndarray) -> np.ndarray:
        if self.span is None:
            return curve
        low, high = self.span
        points = np.arange(POINTS)
        inside = (points >= math.floor(low)) & (points <= math.ceil(high))
        adjusted = np.where(inside, np.clip(self.a * curve + self.c, 0.0, 1.0), curve)
        return monotone(adjusted)


# -- component prices --------------------------------------------------------

class ComponentQuotes:
    """The agent's own supplier quotes per line (LIPP) and market indicators."""

    def __init__(self) -> None:
        self.latest: dict[tuple[str, str], tuple[int, list[tuple[int, float]]]] = {}
        self.capacity: dict[tuple[str, str], float] = {}

    def observe(self, observation: DayObservation) -> None:
        today: dict[tuple[str, str], dict[int, float]] = {}
        for offer in observation.supplier_offers:
            if offer.kind == "none" or offer.unit_price <= 0:
                continue
            today.setdefault((offer.supplier_id, offer.component_id), {})[offer.due_day] = float(offer.unit_price)
        for line, points in today.items():
            self.latest[line] = (observation.day, sorted(points.items()))
        if observation.market_report is not None:
            for row in observation.market_report.supplier_lines:
                if row.mean_capacity_nano is not None:
                    self.capacity[(row.supplier_id, row.component_id)] = row.mean_capacity_nano / 1e9

    def lipp(self, supplier: str, component: str, due: int) -> tuple[float, int, int]:
        """(price / base, distance to the nearest quoted due day, days since
        the quotes); the base price without quotes."""
        entry = self.latest.get((supplier, component))
        base = BASE_MICROCREDITS[component]
        if entry is None:
            return 1.0, 99, 99
        day, points = entry
        dues = np.array([d for d, _ in points], dtype=float)
        prices = np.array([p for _, p in points]) / base
        return float(np.interp(due, dues, prices)), int(np.min(np.abs(dues - due))), day

    def features(
        self, market: PcMarketState, day: int, supplier: str, component: str, send_lag: int, due: int
    ) -> np.ndarray:
        lipp, nearest, quoted_day = self.lipp(supplier, component, due)
        history = market.demand_history

        def demand(window: int) -> float:
            rows = history[-window:]
            return sum(row.get(component, 0) for row in rows) / max(1, len(rows))

        q = market.q_mean
        return np.array([
            day, send_lag, due - (day + send_lag), due - day, lipp, nearest,
            day - quoted_day if quoted_day <= day else 99,
            self.capacity.get((supplier, component), float("nan")),
            demand(5), demand(20), demand(40),
            sum(q.values()), q.get("low", float("nan")), q.get("mid", float("nan")), q.get("high", float("nan")),
        ], dtype=float)


class ResidualTrees:
    """RCPP residual trees per component type (JSON: nested nodes)."""

    def __init__(self, path: Path) -> None:
        self.trees = json.loads(path.read_text())["trees"]

    def predict(self, component: str, features: np.ndarray) -> float:
        node = self.trees.get(component_type(component))
        if node is None:
            return 0.0
        while "value" not in node:
            x = features[node["feature"]]
            node = node["left"] if (not math.isnan(x) and x <= node["threshold"]) or (math.isnan(x) and node.get("nan_left", True)) else node["right"]
        return float(node["value"])


__all__ = [
    "AffineCalibration", "ComponentQuotes", "K_NEIGHBOURS", "NOMINAL", "PC_FEATURES", "POINTS", "PRODUCT_IDS",
    "PcMarketState", "PcPriceModel", "RESIDUAL_FEATURES", "ResidualTrees", "SEND_LAGS", "SMOOTH_DAYS",
    "component_type", "monotone", "relative_grid", "relative_index", "win_curve",
]
