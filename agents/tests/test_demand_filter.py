"""Demand filter on a day without RFQ issuance (draft3's day 0)."""

from __future__ import annotations

from tac_scm_gallery.demand_filter import DemandFilter


def test_day_without_issuance_advances_without_conditioning() -> None:
    skipped, conditioned = DemandFilter(particles=500), DemandFilter(particles=500)
    prior = {name: segment.mean() for name, segment in skipped.segments.items()}
    skipped.observe({"low": 0, "mid": 0, "high": 0})
    conditioned.segments["low"].observe(0)
    for name, segment in skipped.segments.items():
        assert segment.days == 1
        assert abs(segment.mean() - prior[name]) < 0.05 * prior[name]
    # A zero count taken as data would pull the posterior to the bottom of the range.
    assert conditioned.segments["low"].mean() < 0.6 * prior["low"]


def test_counts_still_condition() -> None:
    demand = DemandFilter(particles=500)
    for _ in range(20):
        demand.observe({"low": 90, "mid": 100, "high": 30})
    assert demand.segments["low"].mean() > 70 and demand.segments["high"].mean() < 45
