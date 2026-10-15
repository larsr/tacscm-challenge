"""Bounded NaRC-inspired procurement MDP over public observations."""

from __future__ import annotations

from dataclasses import dataclass

from tac_scm_sdk.contracts import (
    DayActionSet,
    DayObservation,
    SupplierOffer,
    SupplierOrderCommand,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, SUPPLIERS

from .botticelli import (
    BotticelliScenarioKernel,
    _component_need,
    _forecast_products,
    _open_orders,
    _position,
    _supply,
)
from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol


HORIZON_DAYS = 5
MINIMUM_BALANCE = -16
MAXIMUM_BALANCE = 32
OPERATION_BUDGET = 2_048
WAIT_SHORTAGE_LIMIT = 16
RFQ_COST = 2_000_000
HOLDING_COST = 8_000_000
SHORTAGE_COST = 180_000_000
TERMINAL_SHORTAGE_COST = 400_000_000
TERMINAL_EXCESS_COST = 20_000_000
MAXIMUM_POSITION = 224
MAXIMUM_COMPONENT_POSITION = 32
DEBT_FLOOR = -95_000_000_000
PRICE_OUTCOMES_PPM = ((850_000, 250_000), (1_000_000, 500_000), (1_200_000, 250_000))


class BudgetExceeded(RuntimeError):
    """Raised before a planner would exceed its public operation bound."""


@dataclass(slots=True)
class OperationCounter:
    budget: int
    used: int = 0

    def tick(self) -> None:
        if self.used >= self.budget:
            raise BudgetExceeded
        self.used += 1


@dataclass(frozen=True, slots=True)
class ActionValues:
    accept: int
    probe: int
    wait: int
    chosen: str


def clamp_balance(value: int) -> int:
    return min(MAXIMUM_BALANCE, max(MINIMUM_BALANCE, value))


def terminal_value(balance: int) -> int:
    bounded = clamp_balance(balance)
    if bounded < 0:
        return bounded * TERMINAL_SHORTAGE_COST
    return -bounded * TERMINAL_EXCESS_COST


def price_outcomes(base_price: int) -> tuple[tuple[int, int], ...]:
    return tuple(
        (max(1, base_price * multiplier // 1_000_000), probability)
        for multiplier, probability in PRICE_OUTCOMES_PPM
    )


class ProcurementBellman:
    """Finite public-state dynamic program for one component."""

    def __init__(
        self,
        *,
        base_price: int,
        daily_need: int,
        probe_quantity: int,
        reliability_ppm: int,
        counter: OperationCounter,
        allow_wait: bool = True,
    ) -> None:
        self.base_price = base_price
        self.daily_need = max(1, daily_need)
        self.probe_quantity = max(1, probe_quantity)
        self.reliability_ppm = min(950_000, max(600_000, reliability_ppm))
        self.counter = counter
        self.allow_wait = allow_wait
        self._cache: dict[tuple[int, int, int, int, int], int] = {}

    @staticmethod
    def _daily_reward(balance: int) -> int:
        return (
            -max(0, -balance) * SHORTAGE_COST
            - max(0, balance) * HOLDING_COST
        )

    def _rollout(self, step: int, balance: int, quantity: int, arrival_step: int) -> int:
        value = 0
        for cursor in range(step, HORIZON_DAYS):
            if cursor == arrival_step:
                balance = clamp_balance(balance + quantity)
            balance = clamp_balance(balance - self.daily_need)
            value += self._daily_reward(balance)
        if arrival_step >= HORIZON_DAYS:
            quantity = 0
        return value + terminal_value(balance + (quantity if arrival_step == HORIZON_DAYS else 0))

    def _accept_value(
        self,
        step: int,
        balance: int,
        quote_price: int,
        quote_quantity: int,
        lead_days: int,
    ) -> int:
        cost = quote_price * quote_quantity
        on_time = self._rollout(step, balance, quote_quantity, step + max(1, lead_days))
        delayed = self._rollout(step, balance, quote_quantity, step + max(1, lead_days) + 2)
        return -cost + (
            self.reliability_ppm * on_time
            + (1_000_000 - self.reliability_ppm) * delayed
        ) // 1_000_000

    def _value(
        self,
        step: int,
        balance: int,
        quote_price: int,
        quote_quantity: int,
        quote_lead: int,
    ) -> int:
        balance = clamp_balance(balance)
        key = (step, balance, quote_price, quote_quantity, quote_lead)
        cached = self._cache.get(key)
        if cached is not None:
            return cached
        self.counter.tick()
        if step >= HORIZON_DAYS:
            result = terminal_value(balance)
            self._cache[key] = result
            return result
        after_demand = clamp_balance(balance - self.daily_need)
        daily = self._daily_reward(after_demand)
        wait = daily + self._value(step + 1, after_demand, 0, 0, 0)
        probe = daily - RFQ_COST + sum(
            probability
            * self._value(
                step + 1,
                after_demand,
                price,
                self.probe_quantity,
                3,
            )
            // 1_000_000
            for price, probability in price_outcomes(self.base_price)
        )
        candidates = [probe]
        if self.allow_wait:
            candidates.append(wait)
        if quote_price > 0 and quote_quantity > 0:
            candidates.append(
                self._accept_value(step, balance, quote_price, quote_quantity, quote_lead)
            )
        result = max(candidates)
        self._cache[key] = result
        return result

    def values(
        self,
        *,
        balance: int,
        quote_price: int = 0,
        quote_quantity: int = 0,
        quote_lead: int = 0,
    ) -> ActionValues:
        bounded = clamp_balance(balance)
        after_demand = clamp_balance(bounded - self.daily_need)
        daily = self._daily_reward(after_demand)
        wait = daily + self._value(1, after_demand, 0, 0, 0)
        probe = daily - RFQ_COST + sum(
            probability
            * self._value(1, after_demand, price, self.probe_quantity, 3)
            // 1_000_000
            for price, probability in price_outcomes(self.base_price)
        )
        accept = -(1 << 120)
        if quote_price > 0 and quote_quantity > 0:
            accept = self._accept_value(0, bounded, quote_price, quote_quantity, quote_lead)
        values = {"accept": accept, "probe": probe}
        if self.allow_wait:
            values["wait"] = wait
        chosen = min(values, key=lambda name: (-values[name], name))
        return ActionValues(accept, probe, wait, chosen)


@dataclass(frozen=True, slots=True)
class ProcurementPlan:
    rfqs: tuple[SupplierRfqCommand, ...]
    orders: tuple[SupplierOrderCommand, ...]
    states: int
    fallback: bool
    accept_value: int
    probe_value: int
    wait_value: int
    value_of_waiting: int
    accepted_cost: int
    accepted_units: int
    projected_shortage: int
    accepted_components: int
    probed_components: int
    waited_components: int


def _component_targets(observation: DayObservation, history: PublicHistoryProtocol) -> dict[str, int]:
    targets = _component_need(observation)
    forecast = _forecast_products(history)
    for product, quantity in forecast.items():
        from tac_scm_sdk.profile import PRODUCTS

        for component in PRODUCTS[product][0]:
            targets[component] += quantity
    return {component: min(24, quantity + 10) for component, quantity in targets.items()}


def _price_baseline(observation: DayObservation, component: str) -> int:
    if observation.market_report is not None:
        for row in observation.market_report.components:
            if row.component_id == component and row.mean_unit_price is not None:
                return row.mean_unit_price
    return BASE_MICROCREDITS[component]


def _reliability(observation: DayObservation, supplier: str) -> int:
    values = [
        row.reputation_ppm
        for row in observation.supplier_reputations
        if row.supplier_id == supplier
    ]
    return values[0] if values else 800_000


def _request_id(component: str, day: int) -> str:
    return f"na-{component}-{day:03d}"


def _narc_request_component(value: str) -> str | None:
    if not value.startswith("na-"):
        return None
    component, separator, day = value[3:].rpartition("-")
    return component if separator and day.isdigit() and component in SUPPLIERS else None


class NarcProcurementKernel:
    """NaRC-inspired bounded procurement with common safe completion policy."""

    strategy_id = "narc-mdp-v1"

    def __init__(self, *, mode: str = "mdp", operation_budget: int = OPERATION_BUDGET) -> None:
        if mode not in {"mdp", "always-accept-cheapest", "never-wait"}:
            raise ValueError("unknown NaRC-inspired procurement mode")
        if operation_budget < 0:
            raise ValueError("operation budget must be non-negative")
        self.mode = mode
        self.operation_budget = operation_budget
        if mode != "mdp":
            self.strategy_id = f"narc-{mode}-v1"
        self.completion = BotticelliScenarioKernel()

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
        try:
            plan = self._procurement(observation, history)
        except BudgetExceeded:
            plan = self._fallback(observation, history)
        orders = _open_orders(observation)
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            self.completion._customer_offers(observation, orders, history, "scenario"),
            plan.rfqs,
            plan.orders,
            self.completion._production(observation, orders, history),
            self.completion._deliveries(observation, orders),
        )
        if plan.fallback:
            mode = "fallback"
        elif plan.waited_components and not plan.rfqs:
            mode = "accept-wait" if plan.orders else "wait"
        elif plan.accepted_components and plan.probed_components:
            mode = "mixed"
        elif plan.accepted_components:
            mode = "accept"
        elif plan.probed_components:
            mode = "probe"
        else:
            mode = "wait"
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                mode,
                "Compare accepting, probing, and waiting by bounded future inventory utility.",
                (
                    "five-day-public-state-mdp",
                    "information-value-procurement",
                    "public-reputation-delivery-model",
                    "shared-2026-completion-policy",
                    "deterministic-cheapest-fallback" if plan.fallback else "bounded-dynamic-programming",
                ),
                (
                    ("dp_states", plan.states),
                    ("operation_budget", self.operation_budget),
                    ("fallback", int(plan.fallback)),
                    ("accept_value", plan.accept_value),
                    ("probe_value", plan.probe_value),
                    ("wait_value", plan.wait_value),
                    ("value_of_waiting", plan.value_of_waiting),
                    ("accepted_cost", plan.accepted_cost),
                    ("accepted_units", plan.accepted_units),
                    ("projected_shortage", plan.projected_shortage),
                    ("accepted_components", plan.accepted_components),
                    ("probed_components", plan.probed_components),
                    ("waited_components", plan.waited_components),
                ),
            ),
        )

    def _procurement(
        self, observation: DayObservation, history: PublicHistoryProtocol
    ) -> ProcurementPlan:
        targets = _component_targets(observation, history)
        supply = _supply(observation)
        shortages = {component: max(0, targets[component] - supply.get(component, 0)) for component in SUPPLIERS}
        requests = {request.rfq_id: request for request in observation.supplier_rfqs}
        offers: dict[str, list[SupplierOffer]] = {component: [] for component in SUPPLIERS}
        for offer in observation.supplier_offers:
            request = requests.get(offer.rfq_id)
            component = _narc_request_component(request.client_rfq_id) if request is not None else None
            if (
                component == offer.component_id
                and offer.kind not in {"none", "probe"}
                and offer.quantity > 0
                and offer.unit_price <= (request.max_unit_price or 0)
            ):
                offers[component].append(offer)
        counter = OperationCounter(self.operation_budget)
        selected: list[SupplierOrderCommand] = []
        probes: list[str] = []
        used_groups: set[str] = set()
        position = _position(observation)
        committed = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        spend = 0
        units = 0
        totals = {"accept": 0, "probe": 0, "wait": 0}
        counts = {"accept": 0, "probe": 0, "wait": 0}
        waiting_advantage = -(1 << 120)
        for component in sorted(SUPPLIERS, key=lambda item: (-shortages[item], item)):
            shortage = shortages[component]
            if shortage <= 0 and not offers[component]:
                continue
            quantity = min(8, max(1, shortage))
            base = _price_baseline(observation, component)
            supplier = min(SUPPLIERS[component])
            bellman = ProcurementBellman(
                base_price=base,
                daily_need=1,
                probe_quantity=quantity,
                reliability_ppm=_reliability(observation, supplier),
                counter=counter,
                allow_wait=(
                    self.mode != "never-wait"
                    and (
                        self.mode != "mdp"
                        or (bool(offers[component]) and shortage <= WAIT_SHORTAGE_LIMIT)
                    )
                ),
            )
            candidates: list[tuple[ActionValues, SupplierOffer | None]] = []
            for offer in sorted(offers[component], key=lambda row: (row.unit_price, row.due_day, row.offer_id)):
                if offer.alternative_group_id in used_groups:
                    continue
                values = bellman.values(
                    balance=supply.get(component, 0) - targets[component],
                    quote_price=offer.unit_price,
                    quote_quantity=min(offer.quantity, max(1, shortage)),
                    quote_lead=max(1, offer.due_day - observation.day),
                )
                candidates.append((values, offer))
            if not candidates:
                candidates.append((bellman.values(balance=supply.get(component, 0) - targets[component]), None))
            if self.mode == "always-accept-cheapest" and offers[component] and shortage > 0:
                cheapest = min(offers[component], key=lambda row: (row.unit_price, row.due_day, row.offer_id))
                values = bellman.values(
                    balance=supply.get(component, 0) - targets[component], quote_price=cheapest.unit_price,
                    quote_quantity=min(cheapest.quantity, shortage), quote_lead=max(1, cheapest.due_day - observation.day),
                )
                chosen_values, chosen_offer, chosen_action = values, cheapest, "accept"
            else:
                chosen_values, chosen_offer = min(
                    candidates,
                    key=lambda row: (-max(row[0].accept, row[0].probe, row[0].wait if self.mode == "mdp" else -(1 << 120)), row[1].offer_id if row[1] else ""),
                )
                chosen_action = chosen_values.chosen
            totals["accept"] += chosen_values.accept if chosen_values.accept > -(1 << 100) else 0
            totals["probe"] += chosen_values.probe
            totals["wait"] += chosen_values.wait
            waiting_advantage = max(waiting_advantage, chosen_values.wait - max(chosen_values.accept, chosen_values.probe))
            if chosen_action == "accept" and chosen_offer is not None:
                cost = chosen_offer.quantity * chosen_offer.unit_price
                if (
                    position + units + chosen_offer.quantity <= MAXIMUM_POSITION
                    and supply.get(component, 0) + chosen_offer.quantity <= MAXIMUM_COMPONENT_POSITION
                    and observation.cash_microcredits - committed - spend - cost >= DEBT_FLOOR
                ):
                    selected.append(SupplierOrderCommand(f"so-{observation.day}-{len(selected)}", chosen_offer.offer_id))
                    used_groups.add(chosen_offer.alternative_group_id)
                    spend += cost
                    units += chosen_offer.quantity
                    counts["accept"] += 1
                    continue
                chosen_action = "probe"
            if chosen_action == "probe" and shortage > 0 and observation.day < 205:
                probes.append(component)
                counts["probe"] += 1
            else:
                counts["wait"] += 1
        rfqs = self._rfqs(observation, shortages, probes, position + units)
        if self.mode == "mdp" and counts["wait"] > 0 and waiting_advantage > 0:
            rfqs = ()
        return ProcurementPlan(
            rfqs, tuple(selected), counter.used, False,
            totals["accept"], totals["probe"], totals["wait"],
            0 if waiting_advantage <= -(1 << 100) else waiting_advantage,
            spend, units, sum(shortages.values()), counts["accept"], counts["probe"], counts["wait"],
        )

    @staticmethod
    def _rfqs(
        observation: DayObservation,
        shortages: dict[str, int],
        components: list[str],
        position: int,
    ) -> tuple[SupplierRfqCommand, ...]:
        result: list[SupplierRfqCommand] = []
        used_suppliers: set[str] = set()
        for component in components:
            suppliers = sorted(
                SUPPLIERS[component],
                key=lambda supplier: (-_reliability(observation, supplier), supplier),
            )
            supplier = next((value for value in suppliers if value not in used_suppliers), None)
            if supplier is None:
                continue
            quantity = min(8, shortages[component], MAXIMUM_POSITION - position - sum(row.quantity for row in result))
            if quantity <= 0:
                continue
            used_suppliers.add(supplier)
            result.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(result)}", _request_id(component, observation.day),
                    supplier, component, quantity, min(219, observation.day + 3 + len(result) % 5),
                    _price_baseline(observation, component) * 6 // 5,
                )
            )
            if len(result) == 8:
                break
        return tuple(result)

    def _fallback(
        self, observation: DayObservation, history: PublicHistoryProtocol
    ) -> ProcurementPlan:
        targets = _component_targets(observation, history)
        supply = _supply(observation)
        shortages = {component: max(0, targets[component] - supply.get(component, 0)) for component in SUPPLIERS}
        requests = {request.rfq_id: request for request in observation.supplier_rfqs}
        eligible = [
            offer for offer in observation.supplier_offers
            if offer.rfq_id in requests
            and _narc_request_component(requests[offer.rfq_id].client_rfq_id) == offer.component_id
            and offer.kind not in {"none", "probe"}
            and offer.quantity > 0
            and offer.unit_price <= (requests[offer.rfq_id].max_unit_price or 0)
            and shortages.get(offer.component_id, 0) > 0
        ]
        chosen: list[SupplierOrderCommand] = []
        used_components: set[str] = set()
        spend = 0
        accepted_units = 0
        for offer in sorted(eligible, key=lambda row: (row.unit_price, row.due_day, row.offer_id)):
            if offer.component_id in used_components:
                continue
            cost = offer.quantity * offer.unit_price
            if observation.cash_microcredits - spend - cost < DEBT_FLOOR:
                continue
            chosen.append(SupplierOrderCommand(f"so-{observation.day}-{len(chosen)}", offer.offer_id))
            used_components.add(offer.component_id)
            spend += cost
            accepted_units += offer.quantity
        probes = [component for component in sorted(SUPPLIERS, key=lambda item: (-shortages[item], item)) if shortages[component] > 0 and component not in used_components]
        rfqs = self._rfqs(observation, shortages, probes, _position(observation) + sum(offer.quantity for offer in eligible if offer.component_id in used_components))
        return ProcurementPlan(
            rfqs, tuple(chosen), self.operation_budget, True, 0, 0, 0, 0, spend, accepted_units,
            sum(shortages.values()), len(chosen), len(rfqs), 0,
        )
