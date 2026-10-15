"""TacTex-inspired v4: TacTex as published, with its learned models.

Machinery follows the publications (see the agent's INSPIRATION.md; D =
Pardoe 2011, H = TacTex-06 handbook chapter); predictions come from
`tactex_v4_models` (C2b, C3, C4, C5, C6, C7, C8) and `tactex_v4_state`
(C1 and features).

Demand manager (D p.73-79, Algorithm 1): today's RFQs and RFQs predicted
for the next 10 days (sampled the way the server generates them, counts
from C1) all start without a bid; repeatedly the price of the RFQ with the
largest expected profit gain per extra computer (per cycle when yesterday
used more than 90% of the cycles) is lowered one step, while the expected
production (wins as partial orders) fits the factory; component cost is
replacement cost. Production is planned as late as possible by profit and
today's spare cycles are filled by pulling work forward; orders are
delivered from stock earliest due first.

Supply manager (D p.80-85): projected component needs (the plan, plus a
fraction of predicted demand for days 11-40) and an inventory threshold
falling to zero between days 195 and 215; for each component the first
shortfall is bought for the cheapest delivery day (C5 with the lead-time
premium of H p.20) unless C6 predicts that waiting is cheaper; shortfalls
within five days use short-lead RFQs priced below replacement cost; spare
RFQ slots probe the least recently observed due days.

Implementer choices where the papers leave a value open are the fields of
`TacTexV4Parameters`, each with its source or reason.
"""

from __future__ import annotations

from dataclasses import dataclass, fields
from collections import defaultdict
import heapq
import json
import os

import numpy as np

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    DayActionSet,
    DayObservation,
    DeliveryLine,
    ProductionLine,
    SupplierOrderCommand,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from .contracts import DecisionExplanation, FeatureVector, KernelDecision, PublicHistoryProtocol
from .tactex_v4_models import NOMINAL, PRODUCT_IDS, AcceptanceFilter, DayFactors, ModelTree, TacTexV4Models
from .tactex_v4_state import SEGMENTS, MarketState
from .demand_filter import SEGMENT_BOUNDS

CAPACITY = 2000  # cycles per day (game rule)
LAST_DAY = 219
MIN_SUPPLY_DUE = 2  # earliest supplier due offset (game rule)
MAX_RFQS_PER_SUPPLIER = 5  # positive supplier RFQs per supplier and day (game rule)
GRID = np.round(np.arange(0.20, 1.40 + 1e-9, 0.005), 4)  # price fractions of nominal


@dataclass(frozen=True)
class TacTexV4Parameters:
    horizon_days: int = 10  # D p.73
    price_step: float = 0.005  # bid grid, fraction of nominal; D Algorithm 1 "discretized" (step not given)
    utilization_switch: float = 0.9  # D Algorithm 1
    inventory_threshold: int = 400  # D p.93: later agents "around 400"
    ramp_start: int = 195  # D p.82
    ramp_end: int = 215  # D p.82
    min_supply_lead: int = 5  # D p.83
    max_supply_lead: int = 40  # D p.83
    lead_premium: float = 0.1  # H p.20: 1 + 0.1 (d - 5) / 35
    supplier_reserve_margin: float = 0.1  # D p.84 "somewhat higher" (value not given)
    far_fraction_cap: float = 1 / 6  # D p.78-79: 0 to 1/6 of predicted demand
    endgame_first: int = 215  # T06 p.9
    endgame_last: int = 217  # T06 p.9
    initial_days: int = 20  # T06 p.8: deliveries for the first 20 days requested on day 0


def parameters_from_environment() -> TacTexV4Parameters:
    raw = os.environ.get("TAC_SCM_GALLERY_TACTEX_V4_PARAMETERS")
    if not raw:
        return TacTexV4Parameters()
    values = json.loads(raw)
    known = {f.name for f in fields(TacTexV4Parameters)}
    unknown = set(values) - known
    if unknown:
        raise ValueError(f"unknown TacTex v4 parameters {sorted(unknown)}")
    return TacTexV4Parameters(**values)


@dataclass
class Candidate:
    rfq_id: str | None  # None for a predicted RFQ
    product: str
    quantity: int
    due: int
    lead: int
    offset: int  # day offset of the RFQ (0 = today)
    reserve: float  # fraction of nominal
    cost: float  # replacement cost per unit, fraction of nominal
    probabilities: np.ndarray  # P(win) at each level, level 0 = reserve (with day factor)
    factor: float = 1.0  # day factor applied to the filter's probabilities
    level: int = -1  # -1 = no bid

    def price(self, level: int, step: float) -> float:
        return self.reserve - level * step

    def expected_units(self, level: int) -> float:
        return 0.0 if level < 0 else self.quantity * float(self.probabilities[level])

    def expected_profit(self, level: int, step: float) -> float:
        return 0.0 if level < 0 else self.expected_units(level) * (self.price(level, step) - self.cost)


class ResourceSchedule:
    """TacTex's greedy production scheduler state (D p.73-77).

    Resources per production day offset t (executed on day + t + 1): free
    cycles, and per component the slack A(t) - U(<= t) between the
    components available by then (inventory, expected and intended
    deliveries) and those already planned for use; plus finished computers.
    Orders are produced as late as possible; what cannot be produced is
    taken from finished stock. Trial fills can be rolled back.
    """

    def __init__(self, span: int, available: dict[str, np.ndarray], pcs: dict[str, int]) -> None:
        self.span = span
        self.cycles = np.full(span, float(CAPACITY))
        self.slack = {c: np.asarray(a, dtype=float).copy() for c, a in available.items()}
        self.pcs = {p: float(q) for p, q in pcs.items()}
        self.plan: dict[tuple[int, str], float] = defaultdict(float)

    def _apply(self, t: int, product: str, units: float) -> None:
        parts, cycles = PRODUCTS[product]
        self.cycles[t] -= units * cycles
        for c in parts:
            self.slack[c][t:] -= units
        self.plan[(t, product)] += units

    def fill(self, product: str, units: float, latest: int, use_stock: bool = True) -> tuple[float, list]:
        parts, cycles = PRODUCTS[product]
        remaining, log = units, []
        for t in range(min(self.span - 1, latest), -1, -1):
            if remaining <= 1e-9:
                break
            room = min(remaining, self.cycles[t] / cycles, *(float(self.slack[c][t:].min()) for c in parts))
            if room > 1e-9:
                self._apply(t, product, room)
                log.append(("make", t, product, room))
                remaining -= room
        if remaining > 1e-9 and use_stock:
            take = min(remaining, self.pcs.get(product, 0.0))
            if take > 0:
                self.pcs[product] -= take
                log.append(("stock", 0, product, take))
                remaining -= take
        return units - remaining, log

    def rollback(self, log: list) -> None:
        for kind, t, product, units in reversed(log):
            if kind == "make":
                self._apply(t, product, -units)
            else:
                self.pcs[product] += units

    def shift_into_today(self) -> None:
        """Pull planned production forward into day 0 to use its spare
        cycles (D p.78)."""
        for t in range(1, self.span):
            for product in PRODUCT_IDS:
                amount = self.plan.get((t, product), 0.0)
                if amount <= 1e-9 or self.cycles[0] <= 0:
                    continue
                parts, cycles = PRODUCTS[product]
                move = min(amount, self.cycles[0] / cycles, *(float(self.slack[c][:t].min()) for c in parts))
                if move <= 1e-9:
                    continue
                self.plan[(t, product)] -= move
                self.plan[(0, product)] += move
                self.cycles[t] += move * cycles
                self.cycles[0] -= move * cycles
                for c in parts:
                    self.slack[c][:t] -= move

    def use(self, component: str, t: int) -> float:
        return sum(units for (day, product), units in self.plan.items() if day == t and component in PRODUCTS[product][0])


class TacTexV4Kernel:
    strategy_id = "gallery-tactex-v4"

    def __init__(self, parameters: TacTexV4Parameters | None = None, models: TacTexV4Models | None = None, seed: int = 0) -> None:
        self.parameters = parameters or TacTexV4Parameters()
        self.models = models or TacTexV4Models.load(os.environ.get("TAC_SCM_GALLERY_TACTEX_V4_MODELS"))
        # Fidelity ablations: each named component takes its
        # untrained form (c3: filter started from the first price report,
        # c4: no price change, c6: the 2005 static price-pattern rule).
        for part in filter(None, os.environ.get("TAC_SCM_GALLERY_TACTEX_V4_ABLATE", "").split(",")):
            if part == "c3":
                self.models.acceptance_prior, self.models.acceptance_transition = {}, {}
            elif part == "c4":
                self.models.price_change = {}
            elif part == "c6":
                self.models.component_change = ModelTree()
            else:
                raise ValueError(f"unknown TacTex v4 ablation {part}")
        self.rng = np.random.default_rng(seed)
        self.state = MarketState()
        self.acceptance = {
            p: AcceptanceFilter(self.models.acceptance_prior.get(p), self.models.acceptance_transition.get(p), self.rng)
            for p in PRODUCT_IDS
        }
        self.day_factors = DayFactors(self.parameters.horizon_days)
        self._offers: dict[str, tuple[str, float, int, float]] = {}  # rfq -> product, price frac, lead, P
        self._last_cycles = 0
        self._replacement: dict[str, float] = {}
        self._intended: dict[str, list[tuple[int, float]]] = {}  # unsent intended deliveries (day, units)
        self.metrics: dict[str, int] = {}

    # ------------------------------------------------------------------
    # Beliefs

    def _update_beliefs(self, observation: DayObservation) -> None:
        self.state.observe(observation)
        won = {o.rfq_id for o in observation.new_customer_orders}
        outcomes: dict[str, list[tuple[float, bool]]] = {p: [] for p in PRODUCT_IDS}
        for rfq_id, (product, price, lead, probability) in self._offers.items():
            outcomes[product].append((price, rfq_id in won))
            self.day_factors.record(observation.day - 1, lead, probability, rfq_id in won)
        for p in PRODUCT_IDS:
            self.acceptance[p].observe(self.state.report[p], self.state.awarded_estimate[p], outcomes[p])

    def _price_change(self, product: str) -> float:
        model = self.models.price_change.get(product)
        if model is None or not model.stumps:
            return 0.0
        return model.predict(self.state.c4_features(product))

    def _supplier_fraction(self, component: str, due: int) -> tuple[str, float]:
        """Cheapest supplier and its predicted price fraction for a due day."""
        best: tuple[str, float] | None = None
        for supplier in SUPPLIERS[component]:
            fraction = self.state.supplier.price_fraction(supplier, component, due)
            if fraction is None:
                initial = self.models.initial_prices.get(component, {})
                fraction = initial.get(f"{self.state.day}:{due}", 1.0)
            if best is None or fraction < best[1]:
                best = (supplier, fraction)
        assert best is not None
        return best

    def _premium(self, lead: int) -> float:
        p = self.parameters
        return 1.0 + p.lead_premium * max(0, lead - p.min_supply_lead) / (p.max_supply_lead - p.min_supply_lead)

    def _replacement_cost(self, day: int) -> dict[str, float]:
        """Per component, in microcredits: the supply manager's lowest price
        found for the first intended delivery (D p.84), from its last run;
        before the first run, the predicted price at the minimum lead."""
        if self._replacement:
            return dict(self._replacement)
        due = min(LAST_DAY, day + self.parameters.min_supply_lead)
        return {c: self._supplier_fraction(c, due)[1] * BASE_MICROCREDITS[c] for c in SUPPLIERS}

    # ------------------------------------------------------------------
    # Demand manager

    def _win_curve(self, product: str, offset: int, shift_per_day: float, lead: int, day: int) -> np.ndarray:
        """The filter's P(win) on the price grid (C3), shifted by the
        predicted price change (C4); the day factor is applied by the caller."""
        return self.acceptance[product].win_probabilities(GRID, shift_per_day * offset)

    def _candidates(self, observation: DayObservation, unit_cost: dict[str, float]) -> list[Candidate]:
        p = self.parameters
        day = observation.day
        shift = {prod: self._price_change(prod) / 10.0 for prod in PRODUCT_IDS}  # per day, D p.98-99
        curves: dict[tuple[str, int], np.ndarray] = {}

        def curve(product: str, offset: int, lead: int) -> np.ndarray:
            key = (product, offset)
            if key not in curves:
                curves[key] = self._win_curve(product, offset, shift[product], lead, day)
            return curves[key]

        def levels(product: str, offset: int, lead: int, reserve: float, cost: float) -> tuple[np.ndarray, float]:
            top = int(np.searchsorted(GRID, reserve, side="right")) - 1
            bottom = max(0, int(np.searchsorted(GRID, cost, side="left")))
            if top < bottom:
                return np.zeros(0), 1.0
            factor = self.day_factors.factor(day, lead)
            return np.clip(curve(product, offset, lead)[bottom: top + 1][::-1] * factor, 0.0, 1.0), factor

        result: list[Candidate] = []
        for rfq in observation.customer_rfqs:
            reserve = rfq.unit_reserve / NOMINAL[rfq.product_id]
            cost = unit_cost[rfq.product_id] / NOMINAL[rfq.product_id]
            lead = rfq.due_day - day
            probs, factor = levels(rfq.product_id, 0, lead, reserve, cost)
            if len(probs):
                result.append(Candidate(rfq.rfq_id, rfq.product_id, rfq.quantity, rfq.due_day, lead, 0, reserve, cost, probs, factor))
        # Predicted RFQs for the next days, sampled as the server does (A06 p.1491).
        forecast = self.state.forecast()
        members: dict[str, list[str]] = {s: [] for s in SEGMENTS}
        for product in PRODUCT_IDS:
            members[self.state.segment(product)].append(product)
        for offset in range(1, p.horizon_days + 1):
            if day + offset + 3 > LAST_DAY:
                break
            for segment in SEGMENTS:
                if not members[segment]:
                    continue
                count = int(self.rng.poisson(max(0.0, float(forecast[segment][offset - 1]))))
                for _ in range(count):
                    product = members[segment][int(self.rng.integers(0, len(members[segment])))]
                    quantity = int(self.rng.integers(1, 21))
                    lead = int(self.rng.integers(3, 13))
                    if day + offset + lead > LAST_DAY:
                        continue
                    reserve = int(self.rng.integers(75, 126)) / 100.0
                    cost = unit_cost[product] / NOMINAL[product]
                    probs, factor = levels(product, offset, lead, reserve, cost)
                    if len(probs):
                        result.append(Candidate(None, product, quantity, day + offset + lead, lead, offset, reserve, cost, probs, factor))
        return result

    def _availability(self, observation: DayObservation, accepted: dict[str, np.ndarray], span: int) -> dict[str, np.ndarray]:
        """Components available to production on day offset t (executed on
        day + t + 1): inventory plus deliveries due by then from placed
        orders, today's accepted offers and the supply manager's intended
        deliveries (D p.82: the Supply Manager commits to them)."""
        day = observation.day
        arrivals = {c: np.zeros(span + 2) for c in SUPPLIERS}
        for order in observation.supplier_orders:
            left = order.quantity - order.delivered_quantity
            if left > 0:
                arrivals[order.component_id][max(0, min(span + 1, order.due_day - day))] += left
        for c in SUPPLIERS:
            arrivals[c][: min(span + 2, len(accepted[c]))] += accepted[c][: span + 2]
            for due, quantity in self._intended.get(c, ()):
                if due >= day:
                    arrivals[c][min(span + 1, due - day)] += quantity
        inventory = {item.item_id: item.quantity for item in observation.component_inventory}
        return {c: inventory.get(c, 0) + np.cumsum(arrivals[c])[1: span + 1] for c in SUPPLIERS}

    def _existing_orders(self, observation: DayObservation, schedule: ResourceSchedule, unit_cost: dict[str, float]) -> list[tuple[str, int]]:
        """D p.74: orders due in one day or less are delivered from stock;
        the remaining orders are scheduled by profit, as late as possible."""
        day = observation.day
        open_orders = [o for o in observation.active_customer_orders if o.status == "open"]
        deliveries = []
        for order in sorted((o for o in open_orders if o.due_day <= day + 1), key=lambda o: (o.due_day, -o.unit_price)):
            if schedule.pcs.get(order.product_id, 0.0) >= order.quantity:
                schedule.pcs[order.product_id] -= order.quantity
                deliveries.append((order.order_id, order.quantity))
        delivered = {order_id for order_id, _ in deliveries}

        def profit(order) -> float:  # type: ignore[no-untyped-def]
            late = max(0, day + 2 - order.due_day) * order.daily_penalty / order.quantity
            return order.unit_price - unit_cost[order.product_id] - late

        for order in sorted((o for o in open_orders if o.order_id not in delivered), key=profit, reverse=True):
            schedule.fill(order.product_id, float(order.quantity), max(0, order.due_day - 2 - day))
        return deliveries

    def _bid(self, candidates: list[Candidate], schedule: ResourceSchedule) -> None:
        """D Algorithm 1: lower the bid with the largest expected profit gain
        per extra computer (per cycle when yesterday used more than 90% of
        the cycles) while the partial orders can be filled."""
        p = self.parameters
        day = self.state.day
        per_cycle = self._last_cycles > p.utilization_switch * CAPACITY

        def ratio(c: Candidate) -> tuple[float, float] | None:
            nxt = c.level + 1
            if nxt >= len(c.probabilities):
                return None
            d_profit = c.expected_profit(nxt, p.price_step) - c.expected_profit(c.level, p.price_step)
            d_units = c.expected_units(nxt) - c.expected_units(c.level)
            if d_profit <= 0 or d_units <= 0:
                return None
            return d_profit / (d_units * PRODUCTS[c.product][1] if per_cycle else d_units), d_units

        heap: list[tuple[float, int]] = []
        for i, c in enumerate(candidates):
            r = ratio(c)
            if r is not None:
                heap.append((-r[0], i))
        heapq.heapify(heap)
        while heap:
            _, i = heapq.heappop(heap)
            c = candidates[i]
            r = ratio(c)
            if r is None:
                continue
            filled, log = schedule.fill(c.product, r[1], max(0, c.due - 2 - day))
            if filled < r[1] - 1e-9:
                schedule.rollback(log)
                continue
            c.level += 1
            r = ratio(c)
            if r is not None:
                heapq.heappush(heap, (-r[0], i))

    def _production(self, observation: DayObservation, schedule: ResourceSchedule) -> list[tuple[str, int]]:
        """Today's production: day 0 of the plan after the shift, in whole
        units and within the components in stock now."""
        schedule.shift_into_today()
        components = {item.item_id: item.quantity for item in observation.component_inventory}
        capacity = CAPACITY
        produced = []
        for product in sorted(PRODUCT_IDS, key=lambda q: -schedule.plan.get((0, q), 0.0)):
            parts, cycles = PRODUCTS[product]
            units = min(int(schedule.plan.get((0, product), 0.0) + 1e-6), capacity // cycles, *(components.get(c, 0) for c in parts))
            if units <= 0:
                continue
            for c in parts:
                components[c] -= units
            capacity -= units * cycles
            produced.append((product, units))
        self._last_cycles = CAPACITY - capacity
        return produced

    # ------------------------------------------------------------------
    # Supply manager

    def _threshold(self, day: int) -> int:
        p = self.parameters
        if day <= p.ramp_start:
            return p.inventory_threshold
        if day >= p.ramp_end:
            return 0
        return p.inventory_threshold * (p.ramp_end - day) // (p.ramp_end - p.ramp_start)

    def _far_fraction(self, segment: str, rate: float) -> float:
        """D p.78-79: the fraction of predicted demand we expect to win
        "ranges from zero during times of low demand to 1/6 during times of
        moderate or high demand". Our reading: zero at the segment's lower
        demand bound, 1/6 from the middle of its range upward, linear between."""
        low, high = SEGMENT_BOUNDS[segment]
        middle = (low + high) / 2
        return self.parameters.far_fraction_cap * float(np.clip((rate - low) / (middle - low), 0.0, 1.0))

    def _needs(self, observation: DayObservation, schedule: ResourceSchedule) -> dict[str, np.ndarray]:
        """Projected component use per future day (index 0 = today): the
        production plan for the next 10 days, then a conservative share of
        predicted demand for days 11-40 capped by the factory's cycles
        (D p.78-79)."""
        p = self.parameters
        day = observation.day
        span = p.max_supply_lead + 1
        needs = {c: np.zeros(span) for c in SUPPLIERS}
        for (t, product), units in schedule.plan.items():
            if units > 1e-9 and t <= p.horizon_days:
                for c in PRODUCTS[product][0]:
                    needs[c][t] += units
        forecast = self.state.forecast()
        mean_quantity = 10.5  # mean of the uniform 1..20 customer quantity (game rule)
        members = {s: [q for q in PRODUCT_IDS if self.state.segment(q) == s] for s in SEGMENTS}
        for offset in range(p.horizon_days + 1, span):
            if day + offset > LAST_DAY - 2:
                break
            units = {}
            for segment in SEGMENTS:
                if not members[segment]:
                    continue
                rate = float(forecast[segment][min(len(forecast[segment]) - 1, offset - 1)])
                share = self._far_fraction(segment, rate)
                for product in members[segment]:
                    units[product] = share * rate * mean_quantity / len(members[segment])
            cycles = sum(u * PRODUCTS[q][1] for q, u in units.items())
            scale = min(1.0, CAPACITY / cycles) if cycles > 0 else 0.0
            for product, u in units.items():
                for c in PRODUCTS[product][0]:
                    needs[c][offset] += u * scale
        return needs

    def _price_curve(self, component: str, day: int) -> dict[int, tuple[str, float]]:
        """Predicted (supplier, price fraction) for RFQs sent today due in
        5..40 days, the lower of two suppliers (D p.83)."""
        p = self.parameters
        return {
            lead: self._supplier_fraction(component, day + lead)
            for lead in range(p.min_supply_lead, p.max_supply_lead + 1)
            if day + lead <= LAST_DAY
        }

    def _orders(self, observation: DayObservation) -> tuple[list[str], dict[str, np.ndarray]]:
        """Accept the offers to our RFQs (D p.84: all normal offers; 2-day
        offers only when cheaper than the replacement cost and than the
        normal offers for that component). One offer per alternative group:
        on-date (possibly partial), otherwise an earliest-complete
        alternative due within the planning horizon. Returns the accepted
        offer ids and their deliveries per component and day offset."""
        day = observation.day
        span = self.parameters.max_supply_lead + 1
        client = {r.rfq_id: r.client_rfq_id for r in observation.supplier_rfqs}
        groups: dict[str, list] = {}
        for offer in observation.supplier_offers:
            if offer.quantity > 0 and offer.no_offer_reason is None:
                groups.setdefault(offer.alternative_group_id, []).append(offer)
        # An earliest-complete alternative due beyond the planning horizon
        # serves no intended delivery (no use is projected beyond 40 days,
        # D p.82); 2-day RFQs only buy on their requested day (D p.84).
        horizon = day + self.parameters.max_supply_lead
        rank = {"on-date": 0, "complete": 1}
        chosen = []
        for group in groups.values():
            usable = [
                o for o in group
                if o.kind == "on-date"
                or (o.kind == "complete" and o.due_day <= horizon and not client.get(o.rfq_id, "").startswith("t4-r2-"))
            ]
            if usable:
                chosen.append(min(usable, key=lambda o: rank.get(o.kind, 3)))
        normal_price: dict[str, int] = {}
        for offer in chosen:
            if not client.get(offer.rfq_id, "").startswith("t4-r2-"):
                normal_price[offer.component_id] = min(normal_price.get(offer.component_id, offer.unit_price), offer.unit_price)
        accepted, arriving = [], {c: np.zeros(span) for c in SUPPLIERS}
        for offer in chosen:
            if client.get(offer.rfq_id, "").startswith("t4-r2-"):
                ceiling = min(self._replacement.get(offer.component_id, float("inf")), normal_price.get(offer.component_id, float("inf")))
                if offer.unit_price >= ceiling:
                    continue
            accepted.append(offer.offer_id)
            arriving[offer.component_id][max(0, min(span - 1, offer.due_day - day))] += offer.quantity
        return accepted, arriving

    def _supply(self, observation: DayObservation, needs: dict[str, np.ndarray], accepted: dict[str, np.ndarray]) -> tuple[list, list]:
        """Intended deliveries keep each component above the threshold from
        day 5 to 40; each is one order due that day, sent when the price for
        that lead is below every shorter lead (2005 rule) or, with the
        component price-change model, unless waiting is predicted cheaper
        (D p.82-84, 105). 2-day RFQs cover shortfalls inside five days."""
        p = self.parameters
        day = observation.day
        span = p.max_supply_lead + 1
        inventory = {item.item_id: item.quantity for item in observation.component_inventory}
        arriving = {c: accepted[c].copy() for c in SUPPLIERS}
        for order in observation.supplier_orders:
            left = order.quantity - order.delivered_quantity
            if left > 0:
                arriving[order.component_id][max(0, min(span - 1, order.due_day - day))] += left
        if day == 0:
            # Day 0 (T06 p.8): full capacity, equal product mix, first 20 days.
            per_day = CAPACITY / float(np.mean([PRODUCTS[q][1] for q in PRODUCT_IDS])) / len(PRODUCT_IDS)
            for product in PRODUCT_IDS:
                for c in PRODUCTS[product][0]:
                    needs[c][3: 3 + p.initial_days] += per_day
        rfqs: list[tuple[str, str, int, int, int, bool]] = []  # supplier, component, qty, due, limit, two-day
        replacement: dict[str, float] = {}
        use_c6 = self.models.component_change.root is not None and day > 0
        self._intended = {}
        for component in SUPPLIERS:
            curve = self._price_curve(component, day)
            level = float(inventory.get(component, 0))
            intended: list[tuple[int, float]] = []
            short_term = 0.0
            for t in range(span):
                level += arriving[component][t] - needs[component][t]
                if t < p.min_supply_lead:
                    short_term = max(short_term, -level)
                    continue
                if day + t > LAST_DAY:
                    break
                target = self._threshold(day + t)
                if level < target:
                    intended.append((t, target - level))
                    level = target
            if intended and intended[0][0] in curve:
                first = intended[0][0]
                replacement[component] = min(curve[i][1] for i in curve if i <= first) * BASE_MICROCREDITS[component]
            elif curve:
                replacement[component] = min(f for _, f in curve.values()) * BASE_MICROCREDITS[component]
            for lead, quantity in intended:
                if lead not in curve:
                    continue
                supplier, fraction = curve[lead]
                if use_c6:
                    pending = int(arriving[component].sum())
                    later = (
                        self.models.component_change.predict(
                            self.state.c6_features(supplier, component, m, lead - m, pending, inventory.get(component, 0))
                        )
                        for m in range(1, lead - p.min_supply_lead + 1)
                    )
                    send = all(change >= 0 for change in later)
                else:
                    send = all(fraction < curve[i][1] for i in curve if i < lead)
                if send:
                    limit = int(fraction * BASE_MICROCREDITS[component] * (1 + p.supplier_reserve_margin))
                    rfqs.append((supplier, component, int(np.ceil(quantity)), day + lead, limit, False))
                else:
                    self._intended.setdefault(component, []).append((day + lead, quantity))
            if short_term > 0 and day + MIN_SUPPLY_DUE <= LAST_DAY and component in replacement:
                supplier, _ = self._supplier_fraction(component, day + MIN_SUPPLY_DUE)
                rfqs.append((supplier, component, int(np.ceil(short_term)), day + MIN_SUPPLY_DUE, int(replacement[component]), True))
        for component in SUPPLIERS:
            replacement.setdefault(component, self._replacement.get(component, BASE_MICROCREDITS[component]))
        self._replacement = replacement
        # One probe per supplier and day (game rule) on the least recently
        # observed (component, due day) of its lines; D p.81, 83: spare RFQs
        # serve as probes.
        probes = []
        dues = list(range(day + p.min_supply_lead, min(LAST_DAY, day + p.max_supply_lead) + 1))
        if dues:
            for supplier in sorted({s for suppliers in SUPPLIERS.values() for s in suppliers}):
                lines = [c for c, suppliers in SUPPLIERS.items() if supplier in suppliers]
                component, due = min(
                    ((c, d) for c in lines for d in dues),
                    key=lambda cd: (self.state.supplier.last_seen(supplier, cd[0], cd[1]) or -1, cd[1], cd[0]),
                )
                probes.append((supplier, component, 0, due, None, False))
        return rfqs, probes

    # ------------------------------------------------------------------

    def decide(self, observation: DayObservation, features: FeatureVector, history: PublicHistoryProtocol) -> KernelDecision:
        del features, history
        day = observation.day
        self._update_beliefs(observation)
        if day == LAST_DAY:
            self._offers = {}
            return KernelDecision(empty_action(observation), DecisionExplanation(self.strategy_id, "terminal", "No future stage remains.", ("endgame",)))
        p = self.parameters
        replacement = self._replacement_cost(day)
        unit_cost = {prod: sum(replacement[c] for c in PRODUCTS[prod][0]) for prod in PRODUCT_IDS}
        span = p.horizon_days + 13
        orders, accepted = self._orders(observation)
        schedule = ResourceSchedule(span, self._availability(observation, accepted, span), {i.item_id: i.quantity for i in observation.pc_inventory})
        deliveries = self._existing_orders(observation, schedule, unit_cost)
        candidates = self._candidates(observation, unit_cost)
        if p.endgame_first <= day <= p.endgame_last:
            # Endgame (T06 p.9): bid just below reserve where the product can
            # still be delivered; C8 gives P(sale).
            for c in candidates:
                if c.rfq_id is not None and len(c.probabilities):
                    sale = self.models.endgame_reserve_sale.get(c.product)
                    if sale is not None:
                        c.probabilities[0] = sale
                    filled, log = schedule.fill(c.product, c.expected_units(0), max(0, c.due - 2 - day))
                    if filled >= c.expected_units(0) - 1e-9:
                        c.level = 0
                    else:
                        schedule.rollback(log)
        else:
            self._bid(candidates, schedule)
        offers = []
        self._offers = {}
        for c in candidates:
            if c.rfq_id is None or c.level < 0:
                continue
            price = int(round(c.price(c.level, p.price_step) * NOMINAL[c.product]))
            rfq_reserve = next(r.unit_reserve for r in observation.customer_rfqs if r.rfq_id == c.rfq_id)
            price = max(1, min(price, rfq_reserve))
            offers.append(CustomerOfferCommand(f"t4-co-{day}-{len(offers)}", c.rfq_id, price))
            # Day factors compare actual wins with the filter's own expectation.
            raw = self.acceptance[c.product].win_probability(price / NOMINAL[c.product])
            self._offers[c.rfq_id] = (c.product, price / NOMINAL[c.product], c.lead, raw)
        production = self._production(observation, schedule)
        needs = self._needs(observation, schedule)
        rfqs, probes = self._supply(observation, needs, accepted)
        commands = []
        for index, (supplier, component, quantity, due, limit, two_day) in enumerate(rfqs + probes):
            if not day + MIN_SUPPLY_DUE <= due <= LAST_DAY:
                continue
            commands.append(
                SupplierRfqCommand(f"t4-rq-{day}-{index}", f"t4-{'r2' if two_day else 'rf'}-{day}-{index}", supplier, component, min(quantity, 100_000), due, limit)
            )
        commands = _limit_rfqs(commands)
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            day,
            1,
            tuple(offers),
            tuple(commands),
            tuple(SupplierOrderCommand(f"t4-so-{day}-{i}", offer_id) for i, offer_id in enumerate(orders)),
            tuple(ProductionLine(f"t4-pl-{day}-{i}", i, product, units) for i, (product, units) in enumerate(production)),
            tuple(DeliveryLine(f"t4-dl-{day}-{i}", i, order_id, units) for i, (order_id, units) in enumerate(deliveries)),
        )
        self.metrics = {
            "candidates": len(candidates),
            "offers": len(offers),
            "predicted_rfqs": sum(c.rfq_id is None for c in candidates),
            "supplier_rfqs": sum(c.quantity > 0 for c in commands),
            "production_units": sum(u for _, u in production),
            "delivery_units": sum(u for _, u in deliveries),
        }
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "trained" if self.models.sha256 != "untrained" else "bootstrap",
                "TacTex as published: predictive models feeding a greedy bid, production and supply plan.",
                (f"models-sha256:{self.models.sha256[:16]}",),
                tuple(sorted(self.metrics.items())),
            ),
        )


def _limit_rfqs(commands: list[SupplierRfqCommand]) -> list[SupplierRfqCommand]:
    """Game rule: at most five positive RFQs and one probe per supplier and
    day; positive RFQs on the same line and due day are merged."""
    merged: dict[tuple[str, str, int], SupplierRfqCommand] = {}
    probes: dict[str, SupplierRfqCommand] = {}
    for c in commands:
        if c.quantity == 0:
            probes.setdefault(c.supplier_id, c)
            continue
        key = (c.supplier_id, c.component_id, c.due_day)
        if key in merged:
            old = merged[key]
            limit = None if old.max_unit_price is None or c.max_unit_price is None else max(old.max_unit_price, c.max_unit_price)
            merged[key] = SupplierRfqCommand(old.command_id, old.rfq_id, old.supplier_id, old.component_id, min(100_000, old.quantity + c.quantity), old.due_day, limit)
        else:
            merged[key] = c
    per_supplier: dict[str, int] = {}
    result = []
    for c in sorted(merged.values(), key=lambda c: c.due_day):
        if per_supplier.get(c.supplier_id, 0) < MAX_RFQS_PER_SUPPLIER:
            per_supplier[c.supplier_id] = per_supplier.get(c.supplier_id, 0) + 1
            result.append(c)
    return result + list(probes.values())


__all__ = ["TacTexV4Kernel", "TacTexV4Parameters", "parameters_from_environment"]
