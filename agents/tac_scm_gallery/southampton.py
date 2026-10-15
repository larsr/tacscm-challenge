"""SouthamptonSCM-inspired fuzzy coordination over public observations."""

from __future__ import annotations

from dataclasses import dataclass, replace

from tac_scm_sdk.contracts import DayActionSet, DayObservation, empty_action
from tac_scm_sdk.profile import PRODUCTS

from .botticelli import (
    BotticelliScenarioKernel,
    MAXIMUM_POSITION,
    _inventory,
    _nominal_product,
    _open_orders,
    _position,
)
from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol


PPM = 1_000_000


@dataclass(frozen=True, slots=True)
class FuzzyRule:
    name: str
    conditions: tuple[tuple[str, str], ...]
    margin_adjustment_ppm: int
    procurement_buffer_units: int
    offer_limit: int


RULES = (
    FuzzyRule("demand-low", (("demand", "low"),), -50_000, -2, 3),
    FuzzyRule("demand-medium", (("demand", "medium"),), 0, 2, 4),
    FuzzyRule("demand-high", (("demand", "high"),), 80_000, 5, 5),
    FuzzyRule("inventory-low", (("inventory", "low"),), 50_000, 7, 2),
    FuzzyRule("inventory-medium", (("inventory", "medium"),), 0, 2, 4),
    FuzzyRule("inventory-high", (("inventory", "high"),), -100_000, -5, 5),
    FuzzyRule("cash-stressed", (("cash", "high"),), -40_000, -7, 5),
    FuzzyRule("urgent-load", (("urgency", "high"), ("capacity", "high")), 100_000, 5, 1),
    FuzzyRule("capacity-high", (("capacity", "high"),), 70_000, 0, 2),
    FuzzyRule("endgame", (("endgame", "high"),), -130_000, -8, 5),
    FuzzyRule("scarce-boom", (("demand", "high"), ("inventory", "low")), 140_000, 8, 2),
    FuzzyRule("liquidate", (("cash", "high"), ("inventory", "high")), -170_000, -8, 5),
)


@dataclass(frozen=True, slots=True)
class FuzzyControl:
    margin_adjustment_ppm: int
    procurement_buffer_units: int
    offer_limit: int
    fired_rules: int
    dominant_rule: str
    memberships: tuple[tuple[str, int], ...]


def triangular_partition(value: int, low_center: int, middle_center: int, high_center: int) -> tuple[int, int, int]:
    """Return an exact shoulder/triangle partition whose memberships sum to PPM."""

    if not low_center < middle_center < high_center:
        raise ValueError("membership centers must increase")
    if value <= low_center:
        return PPM, 0, 0
    if value < middle_center:
        medium = (value - low_center) * PPM // (middle_center - low_center)
        return PPM - medium, medium, 0
    if value == middle_center:
        return 0, PPM, 0
    if value < high_center:
        high = (value - middle_center) * PPM // (high_center - middle_center)
        return 0, PPM - high, high
    return 0, 0, PPM


def _demand_ratio(observation: DayObservation, history: PublicHistoryProtocol) -> int:
    current = sum(row.quantity for row in observation.customer_rfqs)
    prior = history.observations[-6:-1]
    baseline = sum(sum(row.quantity for row in day.customer_rfqs) for day in prior)
    baseline = baseline // max(1, len(prior))
    return min(2_000_000, current * PPM // max(1, baseline or current or 1))


def _inventory_cover(observation: DayObservation) -> int:
    components = _inventory(observation.component_inventory)
    pcs = sum(row.quantity for row in observation.pc_inventory)
    feasible = sum(min(components.get(component, 0) for component in bom) for bom, _ in PRODUCTS.values())
    demand = sum(row.quantity for row in observation.customer_rfqs) + sum(
        row.quantity for row in _open_orders(observation)
    )
    return min(2_000_000, (pcs + feasible) * PPM // max(4, demand))


def _cash_stress(observation: DayObservation) -> int:
    return min(PPM, max(0, -observation.cash_microcredits) * PPM // 95_000_000_000)


def _urgency(observation: DayObservation) -> int:
    days = min((row.due_day - observation.day for row in _open_orders(observation)), default=12)
    return min(PPM, max(0, 10 - days) * 100_000)


def _capacity_pressure(observation: DayObservation) -> int:
    cycles = 0
    for row in _open_orders(observation):
        if row.due_day <= observation.day + 5:
            cycles += row.quantity * PRODUCTS[row.product_id][1]
    return min(PPM, cycles * PPM // 10_000)


def _endgame(observation: DayObservation) -> int:
    return min(PPM, max(0, observation.day - 170) * PPM // 45)


def public_memberships(observation: DayObservation, history: PublicHistoryProtocol) -> dict[str, dict[str, int]]:
    values = {
        "demand": (_demand_ratio(observation, history), (500_000, 1_000_000, 1_500_000)),
        "inventory": (_inventory_cover(observation), (250_000, 750_000, 1_250_000)),
        "cash": (_cash_stress(observation), (0, 500_000, 1_000_000)),
        "urgency": (_urgency(observation), (0, 500_000, 1_000_000)),
        "capacity": (_capacity_pressure(observation), (0, 500_000, 1_000_000)),
        "endgame": (_endgame(observation), (0, 500_000, 1_000_000)),
    }
    labels = ("low", "medium", "high")
    return {
        name: dict(zip(labels, triangular_partition(value, *centers), strict=True))
        for name, (value, centers) in values.items()
    }


def _strength(rule: FuzzyRule, memberships: dict[str, dict[str, int]]) -> int:
    return min(memberships[variable][label] for variable, label in rule.conditions)


def infer_control(
    memberships: dict[str, dict[str, int]], *, mode: str = "fuzzy"
) -> FuzzyControl:
    if mode not in {"fuzzy", "crisp", "static"}:
        raise ValueError("unknown SouthamptonSCM-inspired controller mode")
    flattened = tuple(
        (f"{variable}_{label}", memberships[variable][label])
        for variable in sorted(memberships)
        for label in ("low", "medium", "high")
    )
    if mode == "static":
        return FuzzyControl(0, 2, 4, 0, "static-midpoint", flattened)
    strengths = tuple((_strength(rule, memberships), rule) for rule in RULES)
    active = tuple(row for row in strengths if row[0] > 0)
    dominant_strength, dominant = min(
        active,
        key=lambda row: (-row[0], -len(row[1].conditions), -abs(row[1].margin_adjustment_ppm), row[1].name),
    )
    del dominant_strength
    if mode == "crisp":
        return FuzzyControl(
            dominant.margin_adjustment_ppm,
            dominant.procurement_buffer_units,
            dominant.offer_limit,
            len(active),
            dominant.name,
            flattened,
        )
    total = sum(strength for strength, _ in active)
    weighted = lambda attribute: sum(  # noqa: E731 - compact exact defuzzification
        strength * getattr(rule, attribute) for strength, rule in active
    ) // total
    return FuzzyControl(
        weighted("margin_adjustment_ppm"),
        weighted("procurement_buffer_units"),
        min(5, max(1, weighted("offer_limit"))),
        len(active),
        dominant.name,
        flattened,
    )


class SouthamptonFuzzyKernel:
    """Minimum-conjunction, zero-order Sugeno coordinator with crisp ablations."""

    strategy_id = "southampton-fuzzy-v1"

    def __init__(self, *, mode: str = "fuzzy") -> None:
        if mode not in {"fuzzy", "crisp", "static"}:
            raise ValueError("unknown SouthamptonSCM-inspired controller mode")
        self.mode = mode
        if mode != "fuzzy":
            self.strategy_id = f"southampton-{mode}-v1"
        self._completion = BotticelliScenarioKernel()

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: PublicHistoryProtocol,
    ) -> KernelDecision:
        del features
        if observation.day == 219:
            return KernelDecision(
                empty_action(observation),
                DecisionExplanation(self.strategy_id, "terminal", "No future stage remains.", ("endgame",)),
            )
        control = infer_control(public_memberships(observation, history), mode=self.mode)
        orders = _open_orders(observation)
        base_rfqs = self._completion._supplier_rfqs(observation, history)
        rfqs = []
        position = _position(observation)
        added = 0
        for request in base_rfqs:
            target = max(1, min(10, request.quantity + control.procurement_buffer_units // 2))
            quantity = min(target, max(0, MAXIMUM_POSITION - position - added))
            if quantity <= 0:
                continue
            rfqs.append(replace(request, quantity=quantity))
            added += quantity
        supplier_orders, marginal, nodes = self._completion._supplier_orders(
            observation, history, "scenario"
        )
        base_offers = self._completion._customer_offers(observation, orders, history, "scenario")
        customer_rfqs = {row.rfq_id: row for row in observation.customer_rfqs}
        offers = []
        for offer in base_offers[: control.offer_limit]:
            rfq = customer_rfqs[offer.rfq_id]
            floor = _nominal_product(rfq.product_id) * 1_100_000 // PPM
            multiplier = min(PPM, max(760_000, 880_000 + control.margin_adjustment_ppm))
            price = min(rfq.unit_reserve, max(floor, rfq.unit_reserve * multiplier // PPM))
            offers.append(replace(offer, unit_price=price))
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            tuple(offers),
            tuple(rfqs),
            supplier_orders,
            self._completion._production(observation, orders, history),
            self._completion._deliveries(observation, orders),
        )
        metrics = (
            ("margin_adjustment_ppm", control.margin_adjustment_ppm),
            ("procurement_buffer_units", control.procurement_buffer_units),
            ("offer_limit", control.offer_limit),
            ("fired_rule_count", control.fired_rules),
            ("supplier_marginal_value", marginal),
            ("planner_nodes", nodes),
        ) + control.memberships
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                self.mode,
                "Coordinate public demand, stock, cash, urgency, capacity, and endgame through frozen fuzzy rules.",
                (
                    f"dominant-rule:{control.dominant_rule}",
                    "minimum-conjunction",
                    "weighted-average-defuzzification" if self.mode == "fuzzy" else f"{self.mode}-ablation",
                    "botticelli-2026-completion-plumbing",
                ),
                metrics,
            ),
        )
