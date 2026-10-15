"""Bayesian filter of customer demand per segment.

The game's customer demand process (rules/; engine-core game.rs,
arithmetic.rs `customer_trend`): each day a segment's RFQ count is Poisson(q); then
trend <- clamp(trend + eps, 0.95, 1.0526), eps ~ U(-0.01, 0.01), and
q <- q x trend; if q leaves [q_min, q_max] it is clamped and trend resets
to 1. Initial q ~ U(q_min, q_max) per segment (SEGMENT_BOUNDS), trend 1;
the demand state is operator-only (not in observations).

A particle filter over (q, trend) per segment with this exact dynamics and
the Poisson likelihood gives the posterior after each day's count, and
forward simulation gives the predictive distribution of future counts.
"""

from __future__ import annotations

from collections.abc import Sequence

import numpy as np

# (q_min, q_max) per segment; the initial mean is uniform on this range.
SEGMENT_BOUNDS = {"low": (25.0, 100.0), "mid": (30.0, 120.0), "high": (25.0, 100.0)}
TREND_LOW, TREND_HIGH = 0.95, 1_052_631_579 / 1e9
STEP = 0.01


class SegmentFilter:
    def __init__(self, low: float, high: float, particles: int = 2000, seed: int = 0) -> None:
        self.low, self.high = low, high
        self.rng = np.random.default_rng(seed)
        self.q = self.rng.uniform(low, high, particles)
        self.trend = np.ones(particles)
        self.weights = np.full(particles, 1.0 / particles)
        self.days = 0

    def _advance(self, q: np.ndarray, trend: np.ndarray, rng: np.random.Generator) -> tuple[np.ndarray, np.ndarray]:
        trend = np.clip(trend + rng.uniform(-STEP, STEP, len(trend)), TREND_LOW, TREND_HIGH)
        q = q * trend
        out = (q < self.low) | (q > self.high)
        q = np.clip(q, self.low, self.high)
        trend = np.where(out, 1.0, trend)
        return q, trend

    def observe(self, count: int) -> None:
        """Condition on today's count, then advance to tomorrow's state."""
        # Poisson log likelihood up to a constant.
        log_w = np.log(self.weights) + count * np.log(self.q) - self.q
        log_w -= log_w.max()
        weights = np.exp(log_w)
        self.weights = weights / weights.sum()
        if 1.0 / np.sum(self.weights**2) < 0.5 * len(self.weights):
            index = self.rng.choice(len(self.weights), len(self.weights), p=self.weights)
            self.q, self.trend = self.q[index], self.trend[index]
            self.weights = np.full(len(self.weights), 1.0 / len(self.weights))
            # Small jitter on q against particle collapse (well below the
            # process noise of one day).
            self.q = np.clip(self.q * np.exp(self.rng.normal(0, 0.005, len(self.q))), self.low, self.high)
        self.advance()

    def advance(self) -> None:
        """Move to tomorrow's state without an observation."""
        self.q, self.trend = self._advance(self.q, self.trend, self.rng)
        self.days += 1

    def mean(self) -> float:
        """Posterior mean of the next day's q."""
        return float(np.sum(self.weights * self.q))

    def forecast(self, horizon: int, seed: int = 1) -> np.ndarray:
        """Expected q for each of the next `horizon` days (day 1 = next day)."""
        rng = np.random.default_rng(seed)
        q, trend = self.q.copy(), self.trend.copy()
        result = np.empty(horizon)
        for day in range(horizon):
            result[day] = float(np.sum(self.weights * q))
            q, trend = self._advance(q, trend, rng)
        return result

    def predictive_log_prob(self, count: int) -> float:
        """log P(count) for the next day under the posterior predictive."""
        from math import lgamma

        log_terms = count * np.log(self.q) - self.q - lgamma(count + 1)
        top = log_terms.max()
        return float(top + np.log(np.sum(self.weights * np.exp(log_terms - top))))


class DemandFilter:
    def __init__(self, particles: int = 2000) -> None:
        self.segments = {name: SegmentFilter(low, high, particles, seed=index) for index, (name, (low, high)) in enumerate(SEGMENT_BOUNDS.items())}

    def observe(self, counts: dict[str, int]) -> None:
        # A day without RFQs in every segment is a day without issuance
        # (draft3's day 0), not a count: under q >= 25 per segment its
        # probability is below e^-75. The demand walk still moves.
        issued = any(counts.get(name, 0) for name in self.segments)
        for name, segment in self.segments.items():
            if issued:
                segment.observe(int(counts.get(name, 0)))
            else:
                segment.advance()

    def forecast(self, horizon: int) -> dict[str, np.ndarray]:
        return {name: segment.forecast(horizon) for name, segment in self.segments.items()}


def counts_from_rfq_ids(day: int, rfq_ids: Sequence[str]) -> dict[str, int]:
    """Today's RFQ count per segment: ids are customer-rfq/DAY/SEGMENT/ORDINAL
    with ordinals 0..count-1 (RFQs due after day 219 are dropped, so the
    highest ordinal + 1 is used)."""
    counts = {name: 0 for name in SEGMENT_BOUNDS}
    for rfq_id in rfq_ids:
        parts = rfq_id.split("/")
        if len(parts) == 4 and parts[1] == str(day) and parts[2] in counts:
            counts[parts[2]] = max(counts[parts[2]], int(parts[3]) + 1)
    return counts


__all__ = ["DemandFilter", "SEGMENT_BOUNDS", "SegmentFilter", "counts_from_rfq_ids"]
