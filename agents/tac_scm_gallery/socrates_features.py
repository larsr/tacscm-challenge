"""Public, decision-time features for Socrates-inspired opponent models."""

from __future__ import annotations

from tac_scm_sdk.contracts import CustomerRfq, DayObservation
from tac_scm_sdk.profile import PRODUCTS

from .botticelli import _nominal_product
from .contracts import PublicHistoryProtocol


PPM = 1_000_000
FEATURE_NAMES = (
    "day_percent",
    "product_index_percent",
    "quantity_percent",
    "lead_percent",
    "reserve_over_nominal_ppm",
    "penalty_per_unit_over_reserve_ppm",
    "recent_low_over_nominal_ppm",
    "recent_high_over_nominal_ppm",
    "rolling_order_level_ppm",
    "current_demand_percent",
    "rolling_demand_percent",
    "reserve_minus_recent_low_ppm",
)
PRODUCT_IDS = tuple(sorted(PRODUCTS))


def _recent_price_ratios(history: PublicHistoryProtocol) -> tuple[int, int]:
    lows: list[int] = []
    highs: list[int] = []
    for observed in history.observations[-3:]:
        report = observed.daily_price_report
        if report is None:
            continue
        for row in report.products:
            nominal = _nominal_product(row.product_id)
            if row.minimum is not None:
                lows.append(row.minimum * PPM // nominal)
            if row.maximum is not None:
                highs.append(row.maximum * PPM // nominal)
    return (
        sum(lows) // len(lows) if lows else PPM,
        sum(highs) // len(highs) if highs else PPM,
    )


def _order_level(observation: DayObservation) -> int:
    report = observation.market_report
    if report is None:
        return 500_000
    rfqs = sum(row.rfq_count for row in report.products)
    orders = sum(row.order_count for row in report.products)
    return min(PPM, orders * PPM // max(1, rfqs))


def rfq_features(
    observation: DayObservation,
    history: PublicHistoryProtocol,
    rfq: CustomerRfq,
) -> tuple[int, ...]:
    recent_low, recent_high = _recent_price_ratios(history)
    current_demand = sum(row.quantity for row in observation.customer_rfqs)
    recent_demand = [
        sum(row.quantity for row in observed.customer_rfqs)
        for observed in history.observations[-5:]
    ]
    rolling_demand = sum(recent_demand) // max(1, len(recent_demand))
    nominal = _nominal_product(rfq.product_id)
    reserve_ratio = rfq.unit_reserve * PPM // nominal
    penalty_ratio = rfq.daily_penalty * PPM // max(1, rfq.quantity * rfq.unit_reserve)
    return (
        observation.day * PPM // 219,
        PRODUCT_IDS.index(rfq.product_id) * PPM // (len(PRODUCT_IDS) - 1),
        rfq.quantity * PPM // 20,
        (rfq.due_day - observation.day) * PPM // 12,
        reserve_ratio,
        penalty_ratio,
        recent_low,
        recent_high,
        _order_level(observation),
        min(PPM, current_demand * PPM // 2_000),
        min(PPM, rolling_demand * PPM // 2_000),
        reserve_ratio - recent_low,
    )


def aggregate_rfq_features(
    observation: DayObservation,
    history: PublicHistoryProtocol,
) -> tuple[int, tuple[int, ...]]:
    rows = [rfq_features(observation, history, rfq) for rfq in observation.customer_rfqs]
    if not rows:
        return 0, (0,) * len(FEATURE_NAMES)
    return len(rows), tuple(sum(row[index] for row in rows) // len(rows) for index in range(len(FEATURE_NAMES)))


def public_daily_label(observation: DayObservation) -> tuple[int, int, int]:
    report = observation.daily_price_report
    if report is None:
        return 0, 0, 0
    ratios: list[int] = []
    spreads: list[int] = []
    for row in report.products:
        if row.minimum is None or row.maximum is None:
            continue
        nominal = _nominal_product(row.product_id)
        ratios.append((row.minimum + row.maximum) * PPM // (2 * nominal))
        spreads.append((row.maximum - row.minimum) * PPM // nominal)
    if not ratios:
        return 0, 0, 0
    return PPM, sum(ratios) // len(ratios), sum(spreads) // len(spreads)
