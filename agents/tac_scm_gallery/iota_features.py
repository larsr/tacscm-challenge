"""Public five-day features and exact tree logic for the Iota reconstruction."""

from __future__ import annotations

from tac_scm_sdk.contracts import CustomerRfq, DayObservation
from tac_scm_sdk.profile import PRODUCTS

from .botticelli import _nominal_product
from .contracts import PublicHistoryProtocol


PPM = 1_000_000
WINDOW = 5
CLASSES = ("singles", "fours", "sixes", "prune")


def demand_class(rfq_count: int) -> int:
    return 0 if rfq_count < 130 else 1 if rfq_count < 220 else 2


def weighted_prices(history: PublicHistoryProtocol, product_id: str) -> tuple[int, int]:
    nominal = _nominal_product(product_id)
    rows: list[tuple[int, int]] = []
    for observed in history.observations[-WINDOW:]:
        report = observed.daily_price_report
        row = None if report is None else next((item for item in report.products if item.product_id == product_id), None)
        if row is not None and row.minimum is not None and row.maximum is not None:
            rows.append((row.minimum, row.maximum))
    if not rows:
        return nominal * 11 // 10, nominal * 5 // 4
    weights = [1 << index for index in range(len(rows))]
    denominator = sum(weights)
    low = sum(value[0] * weight for value, weight in zip(rows, weights, strict=True)) // denominator
    high = sum(value[1] * weight for value, weight in zip(rows, weights, strict=True)) // denominator
    return low, high


def profit_factor(history: PublicHistoryProtocol, rfq: CustomerRfq) -> int:
    _low, high = weighted_prices(history, rfq.product_id)
    nominal = _nominal_product(rfq.product_id)
    price_factor = min(high, rfq.unit_reserve) - nominal
    return price_factor // PRODUCTS[rfq.product_id][1]


def quantile(values: list[int], numerator: int, denominator: int) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = (len(ordered) - 1) * numerator // denominator
    return ordered[index]


def classify(profit: int, due_days: int, sar_ppm: int, thresholds: tuple[int, int, int, int], low_sar_ppm: int, high_sar_ppm: int) -> int:
    t0, t1, t2, t3 = thresholds
    good = sar_ppm >= high_sar_ppm
    no_good = sar_ppm <= low_sar_ppm or not good
    if profit > t0:
        return 0
    if profit > t1:
        if no_good:
            return 0
        return 1 if due_days < 6 else 0
    if profit > t2:
        if no_good:
            return 1
        return 2 if due_days < 6 else 1
    if profit > t3:
        return 2
    return 3


def class_price(mode: int, history: PublicHistoryProtocol, rfq: CustomerRfq) -> int:
    low, high = weighted_prices(history, rfq.product_id)
    nominal = _nominal_product(rfq.product_id)
    floor = nominal * 1_100_000 // PPM
    if mode == 0:
        target = low * 1_020_000 // PPM
    elif mode == 1:
        target = high
    else:
        target = max(high * 1_050_000 // PPM, rfq.unit_reserve * 950_000 // PPM)
    return min(rfq.unit_reserve, max(floor, target))


def forecast_demand(history: PublicHistoryProtocol, observation: DayObservation) -> tuple[int, int]:
    counts = [len(row.customer_rfqs) for row in history.observations[-WINDOW:]]
    current = len(observation.customer_rfqs)
    if len(counts) < 2:
        return demand_class(current), 0
    deltas = [right - left for left, right in zip(counts, counts[1:])]
    projected = max(0, current + sum(deltas) * 3 // len(deltas))
    predicted = demand_class(projected)
    modal = max(range(3), key=lambda value: (sum(demand_class(item) == value for item in counts), -value))
    confidence = sum(demand_class(item) == modal for item in counts) * PPM // len(counts)
    return predicted, confidence
