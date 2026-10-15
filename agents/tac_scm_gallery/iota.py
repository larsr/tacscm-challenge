"""Iota-inspired online-adaptive opportunistic RFQ classifier."""

from __future__ import annotations

from dataclasses import replace
import hashlib
import importlib.resources
import json
from typing import Any

from tac_scm_sdk.contracts import CustomerOfferCommand, DayObservation
from tac_scm_sdk.profile import PRODUCTS

from .botticelli import BotticelliScenarioKernel, MAXIMUM_OPEN_CUSTOMER_UNITS, _inventory, _nominal_product, _open_orders
from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol
from .iota_features import PPM, class_price, classify, demand_class, forecast_demand, profit_factor


OPERATION_BUDGET = 1_024


class IotaClassifierKernel:
    strategy_id = "iota-opportunistic-classifier-v1"

    def __init__(self, *, mode: str = "adaptive", operation_budget: int = OPERATION_BUDGET) -> None:
        if mode not in {"adaptive", "safe", "opportunistic"}:
            raise ValueError("unknown Iota classification mode")
        self.mode = mode
        self.operation_budget = operation_budget
        raw = importlib.resources.files("tac_scm_gallery").joinpath("iota_model_v1.json").read_bytes()
        self.model: dict[str, Any] = json.loads(raw)
        self.model_sha256 = hashlib.sha256(raw).hexdigest()
        self._thresholds = tuple(int(value) for value in self.model["profit_thresholds_microcredits_per_cycle"])
        self._completion = BotticelliScenarioKernel()
        self._previous_classes: dict[str, int] = {}
        self._recent: list[tuple[int, int, int, int]] = []
        self._last_update = -1
        if mode != "adaptive":
            self.strategy_id = f"iota-pure-{mode}-v1"

    def _feedback(self, observation: DayObservation, history: PublicHistoryProtocol) -> tuple[int, int, int, int, int, bool]:
        accepted = {order.rfq_id for order in observation.new_customer_orders}
        singles = [rfq_id for rfq_id, value in self._previous_classes.items() if value == 0]
        self._recent.append((len(singles), sum(rfq_id in accepted for rfq_id in singles), len(observation.customer_rfqs), demand_class(len(observation.customer_rfqs))))
        self._recent = self._recent[-int(self.model["window_days"]):]
        offered = sum(row[0] for row in self._recent)
        ordered = sum(row[1] for row in self._recent)
        sar = ordered * PPM // offered if offered else 0
        predicted, confidence = forecast_demand(history, observation)
        current = demand_class(len(observation.customer_rfqs))
        demand_effect = -1 if predicted < current else 1 if predicted > current else 0
        low, high = int(self.model["safe_acceptance_low_ppm"]), int(self.model["safe_acceptance_high_ppm"])
        sar_effect = -1 if sar <= low else 1 if sar >= high else 0
        total_rfqs = sum(row[2] for row in self._recent)
        singles_share = offered * PPM // max(1, total_rfqs)
        beta = (singles_share + confidence) // 2
        signal = sar_effect + demand_effect
        cap = int(self.model["maximum_threshold_adjustment_ppm"])
        adjustment = max(-cap, min(cap, beta * signal))
        updated = False
        cadence = int(self.model["threshold_update_days"])
        if self.mode == "adaptive" and observation.day > 0 and observation.day % cadence == 0 and observation.day != self._last_update:
            self._thresholds = tuple(value * (PPM + adjustment) // PPM for value in self._thresholds)
            self._last_update = observation.day
            updated = True
        return sar, current, predicted, sar_effect, adjustment, updated

    def decide(self, observation: DayObservation, features: FeatureVector, history: PublicHistoryProtocol) -> KernelDecision:
        base = self._completion.decide(observation, features, history)
        if observation.day == 219:
            return KernelDecision(
                base.action,
                DecisionExplanation(
                    self.strategy_id,
                    "terminal",
                    "No future stage remains.",
                    (f"model-sha256:{self.model_sha256[:16]}", "endgame"),
                ),
            )
        sar, current_demand, predicted_demand, sar_effect, adjustment, updated = self._feedback(observation, history)
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        open_units = sum(order.quantity for order in _open_orders(observation))
        candidates = []
        class_counts = [0, 0, 0, 0]
        demotions = 0
        operations = 0
        for rfq in observation.customer_rfqs:
            due_days = rfq.due_day - observation.day
            profit = profit_factor(history, rfq)
            class_index = classify(profit, due_days, sar, self._thresholds, int(self.model["safe_acceptance_low_ppm"]), int(self.model["safe_acceptance_high_ppm"]))
            if self.mode == "safe" and class_index < 3:
                class_index = 0
            elif self.mode == "opportunistic" and class_index < 3:
                class_index = 2
            class_counts[class_index] += 1
            operations += 2
            if class_index == 3 or not 4 <= due_days <= 12 or rfq.due_day > 219:
                continue
            price = class_price(class_index, history, rfq)
            nominal = _nominal_product(rfq.product_id)
            if price < nominal * 1_100_000 // PPM or (price - nominal) * rfq.quantity <= rfq.daily_penalty:
                continue
            candidates.append((class_index, -profit, rfq.due_day, rfq.rfq_id, rfq, price))
        cycles_left = 2_000
        selected: list[CustomerOfferCommand] = []
        selected_classes: dict[str, int] = {}
        for class_index, _negative_profit, _due, _id, rfq, price in sorted(candidates):
            if open_units + rfq.quantity > MAXIMUM_OPEN_CUSTOMER_UNITS:
                continue
            covered = min(pcs.get(rfq.product_id, 0), rfq.quantity)
            required = rfq.quantity - covered
            bom, cycles = PRODUCTS[rfq.product_id]
            feasible = required * cycles <= cycles_left and all(components.get(component, 0) >= required for component in bom)
            if not feasible:
                demotions += int(class_index < 2)
                continue
            pcs[rfq.product_id] = pcs.get(rfq.product_id, 0) - covered
            for component in bom:
                components[component] -= required
            cycles_left -= required * cycles
            open_units += rfq.quantity
            selected.append(CustomerOfferCommand(f"io-{observation.day}-{len(selected)}", rfq.rfq_id, price))
            selected_classes[rfq.rfq_id] = class_index
            if len(selected) == 8:
                break
        fallback = operations > self.operation_budget
        action = base.action if fallback else replace(base.action, customer_offers=tuple(selected))
        self._previous_classes = {} if fallback else selected_classes
        metrics = [
            ("singles_count", class_counts[0]), ("fours_count", class_counts[1]), ("sixes_count", class_counts[2]), ("pruned_count", class_counts[3]),
            ("sar_ppm", sar), ("current_demand_class", current_demand), ("predicted_demand_class", predicted_demand), ("sar_effect", sar_effect),
            ("threshold_adjustment_ppm", adjustment), ("threshold_updated", int(updated)), ("resource_demotions", demotions),
            ("submitted_customer_offers", len(action.customer_offers)), ("classifier_operations", operations), ("operation_budget", self.operation_budget), ("fallback", int(fallback)),
        ]
        metrics.extend((f"threshold_t{index}", value) for index, value in enumerate(self._thresholds))
        return KernelDecision(
            action,
            DecisionExplanation(self.strategy_id, "fallback" if fallback else self.mode, "Classify RFQs as Singles, Fours, Sixes, or Prune and allocate safe capacity in class order.", (f"model-sha256:{self.model_sha256[:16]}", "five-day-public-window", "own-singles-feedback", "botticelli-2026-completion-plumbing"), tuple(metrics)),
        )
