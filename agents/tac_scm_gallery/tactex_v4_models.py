"""TacTex-inspired v4 predictive models.

Re-implementations of TacTex's learned and predictive components as
published (see the agent's INSPIRATION.md; D = Pardoe 2011,
H = TacTex-06 handbook chapter, M07 = adapting price predictions):

- C3 ``AcceptanceFilter``: per product a SIR particle filter whose particles
  are normal distributions of the lowest competing price (fraction of the
  product's nominal value); P(win | bid) is the mixture CDF (D p.96-98).
  Prior and transition are learned from past games.
- C2b ``DayFactors``: per due lead (3-12) the ratio of actual to expected
  wins over recent offers (D Table 4.3, H p.10).
- C4 ``PriceChangeModel``: additive regression with decision stumps
  (shrinkage 0.7, 200 iterations, M07 fn 3) of the change of a product's
  average sale price over 10 days, from 31 features (D Table 5.1).
- C5 ``SupplierModel``: free fraction 2(1 - price/base) from offers and
  probes per supplier line and due day (D Algorithm 2), interpolated over
  due days; in our engine the posted price is base(1/2 + u/2), so this
  inversion is exact.
- C6 ``ComponentChangeModel``: model of the true offer price minus the C5
  prediction for an RFQ sent m days from now with lead n (D p.105).
- C7/C8 tables from past games: initial supplier prices per (request day,
  due day) and P(sale at reserve) on days 215-217.

Models are trained offline from game records and loaded from a JSON bundle
(agents/MODELS.md); without a bundle every learned part takes its
untrained form (prior from the first price report, zero price change, no
C6 correction, no C7/C8 tables), which is the bootstrap agent TacTex
itself started from.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import json
import math
from pathlib import Path

import numpy as np
from scipy.special import ndtr

from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS

NOMINAL = {p: float(sum(BASE_MICROCREDITS[c] for c in PRODUCTS[p][0])) for p in PRODUCTS}
PRODUCT_IDS = tuple(sorted(PRODUCTS))
LEADS = tuple(range(3, 13))  # customer lead times (game rule)
PARTICLES = 100  # D p.97


def _normal_cdf(x: np.ndarray) -> np.ndarray:
    return ndtr(x)


def _normal_logpdf(x: np.ndarray, mean: np.ndarray, sd: np.ndarray) -> np.ndarray:
    z = (x - mean) / sd
    return -0.5 * z * z - np.log(sd) - 0.5 * math.log(2 * math.pi)


# --------------------------------------------------------------------------
# Additive regression with decision stumps (C4) and model trees (C6)


@dataclass
class StumpEnsemble:
    """f(x) = base + sum of stumps; each stump (feature, threshold, left, right)."""

    base: float = 0.0
    stumps: list[tuple[int, float, float, float]] = field(default_factory=list)

    def predict(self, x: np.ndarray) -> float:
        value = self.base
        for feature, threshold, left, right in self.stumps:
            value += left if x[feature] <= threshold else right
        return value

    @classmethod
    def from_json(cls, raw: dict | None) -> "StumpEnsemble":
        if not raw:
            return cls()
        return cls(float(raw["base"]), [tuple(s) for s in raw["stumps"]])  # type: ignore[misc]


@dataclass
class ModelTree:
    """Model tree: internal nodes split on (feature, threshold); leaves are
    linear models (intercept, coefficients). Stored as nested dicts."""

    root: dict | None = None

    def predict(self, x: np.ndarray) -> float:
        node = self.root
        if node is None:
            return 0.0
        while "leaf" not in node:
            node = node["left"] if x[node["feature"]] <= node["threshold"] else node["right"]
        intercept, coefficients = node["leaf"]
        return float(intercept + sum(c * x[i] for i, c in coefficients))


# --------------------------------------------------------------------------
# C3: acceptance particle filter


class AcceptanceFilter:
    """Belief over the lowest competing price for one product.

    Particle k is Normal(mean_k, sd_k) of the lowest price offered by the
    other agents on an RFQ, as a fraction of nominal. Each day the filter
    conditions on yesterday's reported lowest and highest winning prices
    (order statistics of the day's winning prices) and on the outcome of
    each of our own offers (won: competing price above ours; lost: below),
    then resamples and moves particles by the learned day-to-day change.
    """

    def __init__(self, prior: list[tuple[float, float]] | None, transition: list[tuple[float, float]] | None, rng: np.random.Generator) -> None:
        self.rng = rng
        self.transition = np.array(transition, dtype=float) if transition else None
        if prior:
            sample = np.array(prior, dtype=float)[rng.integers(0, len(prior), PARTICLES)]
            self.mean, self.sd = sample[:, 0].copy(), np.maximum(sample[:, 1], 1e-3)
            self.initialized = True
        else:
            self.mean = np.ones(PARTICLES)
            self.sd = np.full(PARTICLES, 0.1)
            self.initialized = False
        self.weights = np.full(PARTICLES, 1.0 / PARTICLES)

    def _initialize_from_report(self, low: float, high: float) -> None:
        # Untrained bootstrap: particles spread over the first reported range.
        self.mean = self.rng.uniform(low, max(high, low + 1e-3), PARTICLES)
        self.sd = np.full(PARTICLES, max((high - low) / 4, 0.01))
        self.initialized = True

    def observe(self, report: tuple[float, float] | None, awarded: int, outcomes: list[tuple[float, bool]]) -> None:
        if report is not None and not self.initialized:
            self._initialize_from_report(*report)
        log_w = np.log(self.weights)
        if report is not None and awarded >= 1:
            low, high = report
            n = max(awarded, 2)
            f_low = _normal_logpdf(np.full(PARTICLES, low), self.mean, self.sd)
            f_high = _normal_logpdf(np.full(PARTICLES, high), self.mean, self.sd)
            between = _normal_cdf((high - self.mean) / self.sd) - _normal_cdf((low - self.mean) / self.sd)
            log_w = log_w + f_low + (f_high if high > low else 0.0) + (n - 2) * np.log(np.maximum(between, 1e-12))
        for price, won in outcomes:
            cdf = _normal_cdf((price - self.mean) / self.sd)
            log_w = log_w + np.log(np.maximum(1.0 - cdf if won else cdf, 1e-12))
        if not np.isfinite(log_w.max()):
            # No particle explains the day (all likelihoods zero): keep the
            # belief and only move it.
            log_w = np.log(self.weights)
        log_w = np.where(np.isfinite(log_w), log_w, -np.inf)
        log_w -= log_w.max()
        weights = np.exp(log_w)
        self.weights = weights / weights.sum()
        index = self.rng.choice(PARTICLES, PARTICLES, p=self.weights)
        self.mean, self.sd = self.mean[index], self.sd[index]
        self.weights = np.full(PARTICLES, 1.0 / PARTICLES)
        if self.transition is not None:
            moves = self.transition[self.rng.integers(0, len(self.transition), PARTICLES)]
            self.mean = self.mean + moves[:, 0]
            self.sd = np.maximum(self.sd * np.exp(moves[:, 1]), 1e-3)

    def win_probability(self, price_fraction: float, shift: float = 0.0) -> float:
        """P(lowest competing price > our price), the mixture CDF."""
        cdf = _normal_cdf((price_fraction - self.mean - shift) / self.sd)
        return float(np.dot(self.weights, 1.0 - cdf))

    def win_probabilities(self, price_fractions: np.ndarray, shift: float = 0.0) -> np.ndarray:
        z = (price_fractions[:, None] - self.mean[None, :] - shift) / self.sd[None, :]
        return (1.0 - _normal_cdf(z)) @ self.weights


# --------------------------------------------------------------------------
# C2b: day factors


class DayFactors:
    """Actual over expected wins per due lead, over the recent offers."""

    def __init__(self, window: int = 10) -> None:
        self.window = window  # recent days; the planning horizon (D p.73)
        self.rows: list[tuple[int, int, float, bool]] = []  # day, lead, p, won
        self._cache: dict[tuple[int, int], float] = {}

    def record(self, day: int, lead: int, probability: float, won: bool) -> None:
        self.rows.append((day, lead, probability, won))
        self._cache.clear()

    def factor(self, day: int, lead: int) -> float:
        key = (day, lead)
        if key not in self._cache:
            self.rows = [r for r in self.rows if day - r[0] <= self.window]
            recent = [r for r in self.rows if r[1] == lead]
            expected = sum(r[2] for r in recent)
            actual = sum(r[3] for r in recent)
            # One pseudo-observation at the model's own expectation keeps the
            # ratio at 1 without evidence.
            self._cache[key] = (actual + 1.0) / (expected + 1.0)
        return self._cache[key]


# --------------------------------------------------------------------------
# C5: supplier model


class SupplierModel:
    """Posted-price belief per supplier line and due day.

    Each offer or probe reveals the posted price for (line, due day) on the
    day it was quoted; free fraction = 2(1 - price/base). Predictions for an
    unobserved due day interpolate linearly between the nearest observed
    due days (the differencing of D Algorithm 2 under cumulative supply)
    and hold the last value beyond them.
    """

    def __init__(self) -> None:
        self.quotes: dict[tuple[str, str], dict[int, tuple[int, float]]] = {}

    def observe(self, day: int, supplier: str, component: str, due_day: int, unit_price: int) -> None:
        self.quotes.setdefault((supplier, component), {})[due_day] = (day, unit_price / BASE_MICROCREDITS[component])

    def last_seen(self, supplier: str, component: str, due_day: int) -> int | None:
        quote = self.quotes.get((supplier, component), {}).get(due_day)
        return quote[0] if quote else None

    def price_fraction(self, supplier: str, component: str, due_day: int) -> float | None:
        quotes = self.quotes.get((supplier, component))
        if not quotes:
            return None
        if due_day in quotes:
            return quotes[due_day][1]
        dues = sorted(quotes)
        lower = [d for d in dues if d < due_day]
        upper = [d for d in dues if d > due_day]
        if lower and upper:
            a, b = lower[-1], upper[0]
            w = (due_day - a) / (b - a)
            return (1 - w) * quotes[a][1] + w * quotes[b][1]
        return quotes[(lower or upper)[-1 if lower else 0]][1]

    def free_fraction(self, supplier: str, component: str, due_day: int) -> float | None:
        fraction = self.price_fraction(supplier, component, due_day)
        return None if fraction is None else 2.0 * (1.0 - fraction)


# --------------------------------------------------------------------------
# Bundle


@dataclass
class TacTexV4Models:
    acceptance_prior: dict[str, list[tuple[float, float]]] = field(default_factory=dict)
    acceptance_transition: dict[str, list[tuple[float, float]]] = field(default_factory=dict)
    price_change: dict[str, StumpEnsemble] = field(default_factory=dict)
    component_change: ModelTree = field(default_factory=ModelTree)
    initial_prices: dict[str, dict[str, float]] = field(default_factory=dict)  # component -> "rday:due" -> fraction
    endgame_reserve_sale: dict[str, float] = field(default_factory=dict)  # product -> P(sale at reserve)
    sha256: str = "untrained"

    @classmethod
    def load(cls, path: str | None) -> "TacTexV4Models":
        if not path:
            return cls()
        import hashlib

        raw_bytes = Path(path).read_bytes()
        raw = json.loads(raw_bytes)
        if raw.get("schema") != "tac-scm-tactex-v4-models/v1":
            raise ValueError("unknown TacTex v4 model schema")
        return cls(
            acceptance_prior={k: [tuple(x) for x in v] for k, v in raw.get("acceptance_prior", {}).items()},  # type: ignore[misc]
            acceptance_transition={k: [tuple(x) for x in v] for k, v in raw.get("acceptance_transition", {}).items()},  # type: ignore[misc]
            price_change={k: StumpEnsemble.from_json(v) for k, v in raw.get("price_change", {}).items()},
            component_change=ModelTree(raw.get("component_change")),
            initial_prices=raw.get("initial_prices", {}),
            endgame_reserve_sale=raw.get("endgame_reserve_sale", {}),
            sha256=hashlib.sha256(raw_bytes).hexdigest(),
        )


__all__ = [
    "AcceptanceFilter",
    "DayFactors",
    "LEADS",
    "ModelTree",
    "NOMINAL",
    "PRODUCT_IDS",
    "StumpEnsemble",
    "SupplierModel",
    "TacTexV4Models",
]
