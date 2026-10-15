"""Lawful public features and exact inference for the Mertacor reconstruction."""

from __future__ import annotations

from tac_scm_sdk.contracts import CustomerRfq, DayObservation
from tac_scm_sdk.profile import PRODUCTS

from .botticelli import _nominal_product
from .contracts import PublicHistoryProtocol


PPM = 1_000_000
CAPACITY_CYCLES = 2_000
DEMAND_SCALE = 3_500
FEATURE_COUNT = 9
FACTORS_PPM = tuple(range(900_000, 1_050_001, 10_000))


def recent_price_ratios(history: PublicHistoryProtocol, product_id: str) -> tuple[int, int]:
    lows: list[int] = []
    highs: list[int] = []
    nominal = _nominal_product(product_id)
    for observed in history.observations[-5:]:
        report = observed.daily_price_report
        if report is None:
            continue
        row = next((item for item in report.products if item.product_id == product_id), None)
        if row is None:
            continue
        if row.minimum is not None:
            lows.append(row.minimum * PPM // nominal)
        if row.maximum is not None:
            highs.append(row.maximum * PPM // nominal)
    return (
        sum(lows) // len(lows) if lows else 1_200_000,
        sum(highs) // len(highs) if highs else 1_400_000,
    )


def acceptance_features(
    observation: DayObservation,
    history: PublicHistoryProtocol,
    rfq: CustomerRfq,
    unit_price: int,
) -> tuple[int, ...]:
    nominal = _nominal_product(rfq.product_id)
    recent_low, recent_high = recent_price_ratios(history, rfq.product_id)
    total_quantity = sum(row.quantity for row in observation.customer_rfqs)
    return (
        observation.day * PPM // 219,
        rfq.due_day * PPM // 219,
        min(2 * PPM, rfq.unit_reserve * PPM // nominal),
        min(PPM, rfq.quantity * PPM // 20),
        min(2 * PPM, nominal // 4_000),
        recent_high,
        recent_low,
        min(PPM, total_quantity * PPM // DEMAND_SCALE),
        min(2 * PPM, unit_price * PPM // nominal),
    )


def control_state(observation: DayObservation) -> tuple[int, int, int]:
    won = sum(
        order.quantity * PRODUCTS[order.product_id][1]
        for order in observation.new_customer_orders
    )
    queued = sum(
        order.quantity * PRODUCTS[order.product_id][1]
        for order in observation.active_customer_orders
        if order.status == "open"
    )
    demand = sum(rfq.quantity for rfq in observation.customer_rfqs)
    return (
        min(2 * PPM, won * PPM // CAPACITY_CYCLES),
        min(4 * PPM, queued * PPM // CAPACITY_CYCLES),
        min(2 * PPM, demand * PPM // DEMAND_SCALE),
    )


def squash_ppm(value: int) -> int:
    """Exact odd saturating activation used by the recurrent policy."""
    return value * PPM // (PPM + abs(value))


def logistic_ppm(weights_ppm: list[int] | tuple[int, ...], features: tuple[int, ...], lut_ppm: list[int] | tuple[int, ...]) -> int:
    score = weights_ppm[0] + sum(
        weight * feature // PPM
        for weight, feature in zip(weights_ppm[1:], features, strict=True)
    )
    clipped = min(8 * PPM, max(-8 * PPM, score))
    numerator = (clipped + 8 * PPM) * 256
    index, remainder = divmod(numerator, 16 * PPM)
    if index >= 256:
        return int(lut_ppm[256])
    return (int(lut_ppm[index]) * (16 * PPM - remainder) + int(lut_ppm[index + 1]) * remainder) // (16 * PPM)


def policy_step(
    model: dict,
    state: tuple[int, int, int],
    reservoir: tuple[int, ...],
) -> tuple[int, tuple[int, ...], int]:
    active = int(model["active_reservoir_units"])
    inputs = state + (PPM,)
    next_reservoir = []
    operations = 0
    for index in range(active):
        total = sum(
            weight * value // PPM
            for weight, value in zip(model["input_weights_ppm"][index], inputs, strict=True)
        )
        total += sum(
            weight * value // PPM
            for weight, value in zip(model["recurrent_weights_ppm"][index][:active], reservoir[:active], strict=True)
        )
        operations += len(inputs) + active
        next_reservoir.append(squash_ppm(total))
    augmented = state + tuple(next_reservoir) + (PPM,)
    scores = []
    for row in model["output_weights_ppm"]:
        selected = tuple(row[:3]) + tuple(row[3 : 3 + active]) + (row[-1],)
        scores.append(sum(weight * value // PPM for weight, value in zip(selected, augmented, strict=True)))
        operations += len(augmented)
    action = max(range(len(scores)), key=lambda index: (scores[index], -index))
    return action, tuple(next_reservoir), operations
