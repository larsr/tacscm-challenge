"""TacTex-inspired learned public models inside bounded rolling planning."""

from __future__ import annotations

from dataclasses import dataclass, replace
import hashlib
import importlib.resources
import json
from typing import Any

from tac_scm_sdk.contracts import CustomerOfferCommand, DayActionSet, DayObservation, SupplierRfqCommand, empty_action
from tac_scm_sdk.profile import BASE_MICROCREDITS

from .botticelli import (
    BotticelliScenarioKernel,
    MAXIMUM_POSITION,
    _nominal_product,
    _open_orders,
    _position,
)
from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol


PPM = 1_000_000
HORIZON_DAYS = 10
OPERATION_BUDGET = 1_024
PRICE_CANDIDATES_PPM = (780_000, 820_000, 860_000, 900_000, 940_000, 980_000)


@dataclass(frozen=True, slots=True)
class LearnedPublicModel:
    raw: dict[str, Any]
    sha256: str

    @classmethod
    def load(cls) -> "LearnedPublicModel":
        raw_bytes = importlib.resources.files("tac_scm_gallery").joinpath("tactex_model_v1.json").read_bytes()
        value = json.loads(raw_bytes)
        if value.get("schema") != "tac-scm-tactex-public-model/v1":
            raise ValueError("unknown TacTex public model schema")
        return cls(value, hashlib.sha256(raw_bytes).hexdigest())

    @property
    def scale(self) -> int:
        return int(self.raw["coefficient_scale"])

    def regression(self, name: str, features: tuple[int, ...]) -> int:
        coefficients = tuple(int(value) for value in self.raw[name]["coefficients_ppm"])
        if len(coefficients) != len(features):
            raise ValueError("TacTex model feature width differs")
        return sum(coefficient * value for coefficient, value in zip(coefficients, features, strict=True)) // self.scale

    def acceptance(self, price_ratio_ppm: int, lead_days: int) -> int:
        del lead_days
        model = self.raw["offer_acceptance"]
        price_bin = sum(price_ratio_ppm >= int(threshold) for threshold in model["price_ratio_thresholds_ppm"])
        return int(model["probability_ppm"][price_bin])


def _demand_features(observation: DayObservation, history: PublicHistoryProtocol) -> tuple[int, ...]:
    current = sum(row.quantity for row in observation.customer_rfqs)
    recent_observations = history.observations[-5:]
    recent = [sum(row.quantity for row in day.customer_rfqs) for day in recent_observations]
    rolling = sum(recent) // max(1, len(recent))
    return (1, observation.day * 100 // 219, current, rolling - current)


def _reputation(observation: DayObservation) -> int:
    values = tuple(row.reputation_ppm for row in observation.supplier_reputations)
    return sum(values) // max(1, len(values))


def _supplier_observation(observation: DayObservation) -> tuple[int, int, int]:
    offers = tuple(
        row for row in observation.supplier_offers
        if row.kind not in {"none", "probe"} and row.quantity > 0 and row.unit_price > 0
    )
    if not offers:
        return 5, 5, 0
    lead = sum(row.due_day - observation.day for row in offers) // len(offers)
    quantity = sum(row.quantity for row in offers) // len(offers)
    ratio = sum(row.unit_price * PPM // BASE_MICROCREDITS[row.component_id] for row in offers) // len(offers)
    return lead, quantity, ratio


@dataclass(frozen=True, slots=True)
class PredictivePlan:
    action: DayActionSet
    predicted_demand_units: int
    predicted_supplier_ratio_ppm: int
    observed_supplier_ratio_ppm: int
    mean_acceptance_probability_ppm: int
    planned_future_units: int
    operations: int
    fallback: bool
    prior_offered_count: int
    prior_accepted_count: int


class TacTexPredictiveKernel:
    """Frozen trained predictors with a deterministic ten-day greedy plan."""

    strategy_id = "tactex-predictive-planner-v1"

    def __init__(self, *, mode: str = "full", operation_budget: int = OPERATION_BUDGET) -> None:
        if mode not in {"full", "no-predictor", "no-planner"}:
            raise ValueError("unknown TacTex-inspired policy mode")
        if operation_budget < 0:
            raise ValueError("operation budget must be non-negative")
        self.mode = mode
        self.operation_budget = operation_budget
        self.model = LearnedPublicModel.load()
        self._completion = BotticelliScenarioKernel()
        self._previous_customer_rfqs: tuple[str, ...] = ()
        if mode != "full":
            self.strategy_id = f"tactex-{mode}-v1"

    def _predict_demand(self, observation: DayObservation, history: PublicHistoryProtocol) -> int:
        features = _demand_features(observation, history)
        if self.mode == "no-predictor":
            return max(0, features[2])
        adjustment = (
            self.model.regression("demand", features)
            * int(self.model.raw["demand"]["shrinkage_ppm"])
            // PPM
        )
        maximum_adjustment = max(25, features[2] // 2)
        bounded = min(maximum_adjustment, max(-maximum_adjustment, adjustment))
        return max(0, features[2] + bounded)

    def _predict_supplier(self, observation: DayObservation, lead: int, quantity: int) -> int:
        if self.mode == "no-predictor":
            return PPM
        features = (1, observation.day * 100 // 219, lead, quantity, _reputation(observation) // 10_000)
        return min(1_500_000, max(300_000, self.model.regression("supplier_price", features)))

    def _acceptance(self, ratio: int, lead: int) -> int:
        if self.mode == "no-predictor":
            return min(900_000, max(50_000, 2_000_000 - 2 * ratio))
        return self.model.acceptance(ratio, lead)

    def _supplier_rfqs(
        self,
        observation: DayObservation,
        history: PublicHistoryProtocol,
        predicted_demand: int,
    ) -> tuple[SupplierRfqCommand, ...]:
        base = self._completion._supplier_rfqs(observation, history)
        position = _position(observation)
        future_share = min(5, predicted_demand * HORIZON_DAYS // 600)
        result = []
        units = 0
        for request in base:
            quantity = min(10, request.quantity + future_share, max(0, MAXIMUM_POSITION - position - units))
            if quantity <= 0:
                continue
            lead = request.due_day - observation.day
            ratio = self._predict_supplier(observation, lead, quantity)
            maximum = BASE_MICROCREDITS[request.component_id] * min(1_500_000, ratio * 1_050_000 // PPM) // PPM
            result.append(replace(request, quantity=quantity, max_unit_price=max(1, maximum)))
            units += quantity
        return tuple(result)

    def _supplier_orders(self, observation: DayObservation, history: PublicHistoryProtocol):
        filtered = []
        for offer in observation.supplier_offers:
            predicted = self._predict_supplier(observation, offer.due_day - observation.day, max(1, offer.quantity))
            actual = offer.unit_price * PPM // BASE_MICROCREDITS[offer.component_id] if offer.unit_price > 0 else 0
            if offer.kind in {"none", "probe"} or actual <= predicted * 1_050_000 // PPM:
                filtered.append(offer)
        return self._completion._supplier_orders(replace(observation, supplier_offers=tuple(filtered)), history, "scenario")

    def _customer_offers(
        self,
        observation: DayObservation,
        history: PublicHistoryProtocol,
        predicted_demand: int,
    ) -> tuple[tuple[CustomerOfferCommand, ...], int, int]:
        orders = _open_orders(observation)
        base = self._completion._customer_offers(observation, orders, history, "scenario")
        rfqs = {row.rfq_id: row for row in observation.customer_rfqs}
        candidates = []
        operations = 0
        for offer in base:
            rfq = rfqs[offer.rfq_id]
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * 1_100_000 // PPM
            best = None
            for ratio in PRICE_CANDIDATES_PPM:
                operations += 1
                price = min(rfq.unit_reserve, max(floor, rfq.unit_reserve * ratio // PPM))
                actual_ratio = price * PPM // rfq.unit_reserve
                probability = self._acceptance(actual_ratio, rfq.due_day - observation.day)
                expected = probability * max(0, price - nominal) * rfq.quantity // PPM
                row = (-expected, -probability, price, offer.command_id, rfq.rfq_id)
                if best is None or row < best[0]:
                    best = (row, CustomerOfferCommand(offer.command_id, rfq.rfq_id, price), probability)
            if best is not None:
                candidates.append(best)
        planned_future = min(40, predicted_demand * HORIZON_DAYS // 6)
        reserve_slots = 0 if self.mode == "no-planner" else min(3, planned_future // 12)
        limit = max(1, 5 - reserve_slots)
        selected = sorted(candidates, key=lambda row: row[0])[:limit]
        probability = sum(row[2] for row in selected) // max(1, len(selected))
        return tuple(row[1] for row in selected), probability, operations

    def _plan(self, observation: DayObservation, history: PublicHistoryProtocol) -> PredictivePlan:
        predicted_demand = self._predict_demand(observation, history)
        supplier_lead, supplier_quantity, observed_supplier = _supplier_observation(observation)
        predicted_supplier = self._predict_supplier(observation, supplier_lead, supplier_quantity)
        customer_offers, probability, offer_operations = self._customer_offers(observation, history, predicted_demand)
        supplier_rfqs = self._supplier_rfqs(observation, history, predicted_demand)
        supplier_orders, _, supplier_operations = self._supplier_orders(observation, history)
        operations = offer_operations + supplier_operations + len(supplier_rfqs) * HORIZON_DAYS
        accepted = sum(
            order.rfq_id in self._previous_customer_rfqs
            for order in observation.active_customer_orders
        )
        prior_count = len(self._previous_customer_rfqs)
        orders = _open_orders(observation)
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            customer_offers,
            supplier_rfqs,
            supplier_orders,
            self._completion._production(observation, orders, history),
            self._completion._deliveries(observation, orders),
        )
        fallback = operations > self.operation_budget
        if fallback:
            action = self._completion.decide(observation, history.features[-1], history).action
        self._previous_customer_rfqs = tuple(offer.rfq_id for offer in action.customer_offers)
        return PredictivePlan(
            action,
            predicted_demand,
            predicted_supplier,
            observed_supplier,
            probability,
            min(40, predicted_demand * HORIZON_DAYS // 6),
            operations,
            fallback,
            prior_count,
            accepted,
        )

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
        plan = self._plan(observation, history)
        return KernelDecision(
            plan.action,
            DecisionExplanation(
                self.strategy_id,
                "fallback" if plan.fallback else self.mode,
                "Use frozen train-only public predictions inside a bounded ten-day joint plan.",
                (
                    f"model-sha256:{self.model.sha256[:16]}",
                    "ten-day-greedy-resource-plan" if self.mode != "no-planner" else "planner-free-ablation",
                    "predictor-free-ablation" if self.mode == "no-predictor" else "trained-public-models",
                    "botticelli-2026-completion-plumbing",
                ),
                (
                    ("predicted_demand_units", plan.predicted_demand_units),
                    ("predicted_supplier_ratio_ppm", plan.predicted_supplier_ratio_ppm),
                    ("observed_supplier_ratio_ppm", plan.observed_supplier_ratio_ppm),
                    ("acceptance_probability_ppm", plan.mean_acceptance_probability_ppm),
                    ("planned_future_units", plan.planned_future_units),
                    ("planner_operations", plan.operations),
                    ("operation_budget", self.operation_budget),
                    ("fallback", int(plan.fallback)),
                    ("prior_offered_count", plan.prior_offered_count),
                    ("prior_accepted_count", plan.prior_accepted_count),
                ),
            ),
        )
