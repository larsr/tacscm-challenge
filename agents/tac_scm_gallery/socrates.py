"""Socrates-inspired opponent-aware customer bidding with quantized MLPs."""

from __future__ import annotations

from dataclasses import dataclass, replace
import hashlib
import importlib.resources
import json
from typing import Any

from tac_scm_sdk.contracts import CustomerOfferCommand, CustomerRfq, DayObservation

from .botticelli import BotticelliScenarioKernel, _nominal_product
from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol
from .socrates_features import PPM, rfq_features
from .socrates_features import public_daily_label


OPERATION_BUDGET = 1_024


@dataclass(frozen=True, slots=True)
class SocratesModel:
    raw: dict[str, Any]
    sha256: str

    @classmethod
    def load(cls) -> "SocratesModel":
        raw_bytes = importlib.resources.files("tac_scm_gallery").joinpath("socrates_model_v1.json").read_bytes()
        value = json.loads(raw_bytes)
        if value.get("schema") != "tac-scm-socrates-opponent-model/v1":
            raise ValueError("unknown Socrates opponent-model schema")
        return cls(value, hashlib.sha256(raw_bytes).hexdigest())

    def model_for(self, identity: str | None) -> tuple[dict[str, Any], bool]:
        known = self.raw["known_models"]
        if identity is not None and identity in known:
            return known[identity], True
        return self.raw["pooled_model"], False

    @staticmethod
    def infer(model: dict[str, Any], features: tuple[int, ...]) -> tuple[int, int]:
        inputs = features + (PPM,)
        hidden = tuple(
            max(0, sum(weight * value for weight, value in zip(row, inputs, strict=True)) // PPM)
            for row in model["hidden_weights_ppm"]
        )
        augmented = hidden + (PPM,)
        outputs = tuple(
            sum(weight * value for weight, value in zip(row, augmented, strict=True)) // PPM
            for row in model["output_weights_ppm"]
        )
        return min(PPM, max(0, outputs[0])), min(2_000_000, max(300_000, outputs[1]))


def _canonical_label(value: str) -> str:
    return value.split("-", 1)[0]


def public_labels_from_instance(instance_id: str) -> tuple[str, ...]:
    parts = tuple(part for part in instance_id.split(".") if part)
    if len(parts) <= 1 or parts[0] not in {"socrates", "pooled", "aggregate"}:
        return ()
    return tuple(_canonical_label(part) for part in parts[1:6])


def _aggregate_ratio(observation: DayObservation, rfq: CustomerRfq) -> int:
    report = observation.daily_price_report
    if report is not None:
        row = next((item for item in report.products if item.product_id == rfq.product_id), None)
        if row is not None and row.minimum is not None:
            return row.minimum * PPM // _nominal_product(rfq.product_id)
    return 900_000


class SocratesOpponentKernel:
    """Bid below public-roster opponent quantiles with pooled fallback."""

    strategy_id = "socrates-opponent-bidder-v1"

    def __init__(
        self,
        *,
        mode: str = "full",
        public_opponent_labels: tuple[str, ...] = (),
        operation_budget: int = OPERATION_BUDGET,
    ) -> None:
        if mode not in {"full", "pooled", "aggregate"}:
            raise ValueError("unknown Socrates policy mode")
        if operation_budget < 0:
            raise ValueError("Socrates operation budget must be non-negative")
        self.mode = mode
        self.public_opponent_labels = tuple(_canonical_label(value) for value in public_opponent_labels[:5])
        self.operation_budget = operation_budget
        self.model = SocratesModel.load()
        self._completion = BotticelliScenarioKernel()
        self._prior_prediction_probability_ppm = 0
        self._prior_prediction_price_ratio_ppm = 0
        self._prior_prediction_count = 0
        if mode != "full":
            self.strategy_id = f"socrates-{mode}-v1"

    def _opponent_predictions(
        self,
        observation: DayObservation,
        history: PublicHistoryProtocol,
        rfq: CustomerRfq,
    ) -> tuple[list[tuple[int, int]], int, int, int, int]:
        features = rfq_features(observation, history, rfq)
        if self.mode == "aggregate":
            return [(PPM, _aggregate_ratio(observation, rfq))], 0, 0, 1, 1
        labels: tuple[str | None, ...]
        if self.mode == "pooled" or not self.public_opponent_labels:
            labels = (None,)
        else:
            labels = self.public_opponent_labels
        predictions = []
        known_count = 0
        operations = 0
        above_fit_threshold = 0
        reserve_ratio = rfq.unit_reserve * PPM // _nominal_product(rfq.product_id)
        for label in labels:
            model, known = self.model.model_for(label if self.mode == "full" else None)
            probability, price = self.model.infer(model, features)
            operations += 1
            above_fit_threshold += int(probability >= int(model["participation_threshold_ppm"]))
            conditional_price = price + int(model["price_residual_q25_ppm"])
            expected_competitor_price = (
                probability * conditional_price + (PPM - probability) * reserve_ratio
            ) // PPM
            predictions.append((probability, expected_competitor_price))
            known_count += int(known)
        unknown_count = len(labels) - known_count
        return predictions, known_count, unknown_count, operations, above_fit_threshold

    def _customer_offers(
        self,
        observation: DayObservation,
        history: PublicHistoryProtocol,
        base: tuple[CustomerOfferCommand, ...],
    ) -> tuple[tuple[CustomerOfferCommand, ...], dict[str, int]]:
        rfqs = {row.rfq_id: row for row in observation.customer_rfqs}
        result = []
        probabilities = []
        ratios = []
        opponent_ratios = []
        known = 0
        unknown = 0
        operations = 0
        above_fit_threshold = 0
        profit_filtered = 0
        for offer in base:
            rfq = rfqs[offer.rfq_id]
            predictions, known_count, unknown_count, used, above = self._opponent_predictions(observation, history, rfq)
            known += known_count
            unknown += unknown_count
            operations += used
            above_fit_threshold += above
            if not predictions:
                profit_filtered += 1
                continue
            probability, ratio = min(predictions, key=lambda row: (row[1], -row[0]))
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * 1_100_000 // PPM
            price = min(rfq.unit_reserve, nominal * max(300_000, ratio) // PPM - 1)
            if price < floor or (price - nominal) * rfq.quantity <= rfq.daily_penalty:
                profit_filtered += 1
                continue
            result.append(replace(offer, unit_price=price))
            probabilities.append(probability)
            ratios.append(price * PPM // nominal)
            opponent_ratios.append(ratio)
        return tuple(result), {
            "known_model_uses": known,
            "pooled_model_uses": unknown,
            "mean_participation_probability_ppm": sum(probabilities) // max(1, len(probabilities)),
            "minimum_bid_ratio_ppm": min(ratios, default=0),
            "minimum_opponent_ratio_ppm": min(opponent_ratios, default=0),
            "profit_filtered_count": profit_filtered,
            "model_operations": operations,
            "models_above_fit_participation_threshold": above_fit_threshold,
        }

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: PublicHistoryProtocol,
    ) -> KernelDecision:
        base = self._completion.decide(observation, features, history)
        offers, metrics = self._customer_offers(observation, history, base.action.customer_offers)
        observed_participation, observed_price, _spread = public_daily_label(observation)
        fallback = metrics["model_operations"] > self.operation_budget
        action = base.action if fallback else replace(base.action, customer_offers=offers)
        label_source = "explicit-public-launch-roster" if self.public_opponent_labels else "pooled-unknown-roster-fallback"
        explanation_metrics = tuple(metrics.items()) + (
            ("public_opponent_labels", len(self.public_opponent_labels)),
            ("submitted_customer_offers", len(action.customer_offers)),
            ("operation_budget", self.operation_budget),
            ("fallback", int(fallback)),
            ("prior_predicted_participation_ppm", self._prior_prediction_probability_ppm),
            ("prior_predicted_price_ratio_ppm", self._prior_prediction_price_ratio_ppm),
            ("prior_prediction_count", self._prior_prediction_count),
            ("observed_public_participation_ppm", observed_participation),
            ("observed_public_price_ratio_ppm", observed_price),
        )
        self._prior_prediction_probability_ppm = metrics["mean_participation_probability_ppm"]
        self._prior_prediction_price_ratio_ppm = metrics["minimum_opponent_ratio_ppm"]
        self._prior_prediction_count = len(offers)
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "fallback" if fallback else self.mode,
                "Bid below a risk-adjusted public-roster competitor price when profitable and feasible.",
                (
                    f"model-sha256:{self.model.sha256[:16]}",
                    label_source,
                    "twelve-day-production-feasibility",
                    "botticelli-2026-completion-plumbing",
                ),
                explanation_metrics,
                metrics["mean_participation_probability_ppm"],
            ),
        )
