"""Mertacor-inspired adaptive recurrent policy-search seller."""

from __future__ import annotations

from dataclasses import replace
import hashlib
import importlib.resources
import json
from typing import Any

from tac_scm_sdk.contracts import CustomerRfq, DayObservation
from tac_scm_sdk.profile import PRODUCTS

from .botticelli import BotticelliScenarioKernel, _nominal_product
from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol
from .mertacor_features import CAPACITY_CYCLES, FACTORS_PPM, PPM, acceptance_features, control_state, logistic_ppm, policy_step, recent_price_ratios


OPERATION_BUDGET = 1_024


class MertacorPolicyKernel:
    strategy_id = "mertacor-policy-search-v1"

    def __init__(self, *, mode: str = "policy-search", operation_budget: int = OPERATION_BUDGET) -> None:
        if mode not in {"policy-search", "heuristic", "supervised"}:
            raise ValueError("unknown Mertacor policy mode")
        self.mode = mode
        self.operation_budget = operation_budget
        raw = importlib.resources.files("tac_scm_gallery").joinpath("mertacor_model_v1.json").read_bytes()
        self.model: dict[str, Any] = json.loads(raw)
        self.model_sha256 = hashlib.sha256(raw).hexdigest()
        self._completion = BotticelliScenarioKernel()
        self._reservoir = (0,) * int(self.model["active_reservoir_units"])
        self._heuristic_factor = PPM
        if mode != "policy-search":
            self.strategy_id = f"mertacor-{mode}-v1"

    def _factor(self, observation: DayObservation, history: PublicHistoryProtocol, rfqs: tuple[CustomerRfq, ...]) -> tuple[int, int, int]:
        state = control_state(observation)
        if self.mode == "policy-search":
            index, self._reservoir, operations = policy_step(self.model, state, self._reservoir)
            return int(self.model["factors_ppm"][index]), index, operations
        if self.mode == "heuristic":
            self._heuristic_factor += (state[0] - PPM) // 5
            if state[1] > 2 * PPM:
                self._heuristic_factor += 10_000
            if state[1] < PPM:
                self._heuristic_factor -= 5_000
            self._heuristic_factor = min(1_050_000, max(900_000, self._heuristic_factor))
            index = min(range(16), key=lambda row: abs(FACTORS_PPM[row] - self._heuristic_factor))
            return FACTORS_PPM[index], index, 3
        values = [self._portfolio(observation, history, rfqs, factor)[1] for factor in FACTORS_PPM]
        index = max(range(16), key=lambda row: (values[row], -row))
        return FACTORS_PPM[index], index, len(rfqs) * 16

    def _portfolio(self, observation: DayObservation, history: PublicHistoryProtocol, rfqs: tuple[CustomerRfq, ...], factor: int) -> tuple[dict[str, tuple[int, int, int]], int, int]:
        candidates = []
        total_margin = 0
        operations = 0
        for rfq in rfqs:
            nominal = _nominal_product(rfq.product_id)
            _low, high = recent_price_ratios(history, rfq.product_id)
            price = min(rfq.unit_reserve, nominal * high // PPM * factor // PPM)
            if price < nominal * 1_100_000 // PPM or (price - nominal) * rfq.quantity <= rfq.daily_penalty:
                continue
            probability = logistic_ppm(self.model["acceptance_weights_ppm"], acceptance_features(observation, history, rfq, price), self.model["sigmoid_lut_ppm"])
            cycles = rfq.quantity * PRODUCTS[rfq.product_id][1]
            expected_cycles = max(1, probability * cycles // PPM)
            expected_margin = probability * (price - nominal) * rfq.quantity // PPM
            score = expected_margin * PPM // expected_cycles
            candidates.append((-score, rfq.due_day, rfq.rfq_id, price, probability, expected_cycles, expected_margin))
            operations += 10
        selected: dict[str, tuple[int, int, int]] = {}
        used_cycles = 0
        for _score, _due, rfq_id, price, probability, expected_cycles, expected_margin in sorted(candidates):
            if used_cycles + expected_cycles > CAPACITY_CYCLES:
                continue
            selected[rfq_id] = (price, probability, expected_cycles)
            used_cycles += expected_cycles
            total_margin += expected_margin
        return selected, total_margin, operations

    def decide(self, observation: DayObservation, features: FeatureVector, history: PublicHistoryProtocol) -> KernelDecision:
        base = self._completion.decide(observation, features, history)
        rfq_map = {row.rfq_id: row for row in observation.customer_rfqs}
        base_rfqs = tuple(rfq_map[row.rfq_id] for row in base.action.customer_offers)
        factor, action_index, policy_operations = self._factor(observation, history, base_rfqs)
        selected, expected_margin, portfolio_operations = self._portfolio(observation, history, base_rfqs, factor)
        offers = tuple(
            replace(offer, unit_price=selected[offer.rfq_id][0])
            for offer in base.action.customer_offers
            if offer.rfq_id in selected
        )
        expected_cycles = sum(row[2] for row in selected.values())
        operations = policy_operations + portfolio_operations
        fallback = operations > self.operation_budget
        action = base.action if fallback else replace(base.action, customer_offers=offers)
        state = control_state(observation)
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "fallback" if fallback else self.mode,
                "Select a recurrent price factor and rank lawful expected-profit offers per factory cycle.",
                (
                    f"model-sha256:{self.model_sha256[:16]}",
                    "own-offer-train-labels",
                    "expected-partial-cycle-capacity",
                    "botticelli-2026-completion-plumbing",
                ),
                (
                    ("factor_ppm", factor),
                    ("policy_action_index", action_index),
                    ("prior_won_cycles_ratio_ppm", state[0]),
                    ("queued_cycles_ratio_ppm", state[1]),
                    ("demand_quantity_ratio_ppm", state[2]),
                    ("expected_won_cycles", expected_cycles),
                    ("expected_margin_microcredits", expected_margin),
                    ("submitted_customer_offers", len(action.customer_offers)),
                    ("policy_operations", operations),
                    ("operation_budget", self.operation_budget),
                    ("fallback", int(fallback)),
                    ("active_reservoir_units", int(self.model["active_reservoir_units"])),
                ),
            ),
        )
