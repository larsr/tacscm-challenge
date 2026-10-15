"""Botticelli-inspired v4-1: v4 with the jointly optimal supplier-offer set.

Bell et al., *Multi-period Online Optimization in TAC SCM: The Supplier
Offer Acceptance Problem* (TADA 2004; see the agent's INSPIRATION.md):
Botticelli decides which supplier offers to
accept as one first-stage choice of the set of offers that maximises the
expected value of the scenario plan, offers being complements or
substitutes of one another. v1's
``_supplier_orders`` (shared by v3 and v4) implements this as a
leave-one-out test: an offer is kept only if removing that one offer from the
full candidate set lowers the plan value. That test is not the maximiser.
With several offers for the same component (v3's 5 x 250-unit RFQs per
supplier make this the norm), each offer is redundant given the others, so
every leave-one-out marginal is non-positive and all of them are refused
although the set is valuable. In a replayed kit test game (day 100): each
CPU offer alone was worth +0.27 to +0.43 M and the full set +2.07 M over
none, yet each leave-one-out marginal was -0.18 to -0.28 M, and v4 held no
CPUs on 49-88 of days 20-200 (production gaps up to 60 days).

v4-1 chooses the subset S of the candidate offers that maximises the same
plan value ``_plan_value(S)`` subject to the same position and cash limits
(exact feasibility: v1 applied them offer by offer in marginal order; here
the set must satisfy them):

- exactly, by enumerating every subset, when the planner's declared
  operation budget covers it (2^n plan evaluations x scenarios, n the
  number of candidates);
- otherwise by backward elimination from the full set: while removing some
  offer raises the plan value, remove the one that raises it most; while
  the set breaks a limit, remove the one whose removal leaves the highest
  value. Every removal raises the value or restores feasibility, so the
  result is feasible and no single removal improves it (a local optimum of
  the same objective; on a set of mutually redundant offers it keeps the
  useful ones instead of dropping all).

Everything else is v4 (``BotticelliV4Kernel``); v4 and v1 are not edited, so
their behaviour and the fields that use them are unchanged. The fallback
mode (operation budget below the planner's own needs) keeps v1's base-price
rule.
"""

from __future__ import annotations

from tac_scm_sdk.contracts import DayObservation, SupplierOffer, SupplierOrderCommand

from . import botticelli as v1
from .botticelli import _plan_value, _position, _request_component, _scenarios
from .botticelli_v3 import BotticelliV3Parameters
from .botticelli_v4 import BotticelliV4Kernel, BotticelliV4Model, parameters_from_environment, parse_parameters
from .contracts import PublicHistoryProtocol


def candidate_offers(observation: DayObservation) -> tuple[SupplierOffer, ...]:
    """v1's candidates: per alternative group the cheapest valid offer that
    answers one of the agent's own procurement RFQs within its price limit."""
    requests = {request.rfq_id: request for request in observation.supplier_rfqs}
    groups: dict[str, list[SupplierOffer]] = {}
    for offer in observation.supplier_offers:
        request = requests.get(offer.rfq_id)
        if (
            request is None
            or _request_component(request.client_rfq_id) != offer.component_id
            or offer.kind in {"none", "probe"}
            or offer.quantity <= 0
            or offer.unit_price > (request.max_unit_price or 0)
        ):
            continue
        groups.setdefault(offer.alternative_group_id, []).append(offer)
    return tuple(
        min(rows, key=lambda offer: (offer.unit_price, offer.due_day, -offer.quantity, offer.offer_id))
        for _, rows in sorted(groups.items())
    )


def select_offers(
    candidates: tuple[SupplierOffer, ...],
    value,  # type: ignore[no-untyped-def]  # (offers) -> (plan value, nodes)
    feasible,  # type: ignore[no-untyped-def]  # (offers) -> bool
    exact: bool,
) -> tuple[tuple[int, ...], int, int]:
    """Indices of the chosen offers, their plan value and the nodes used.

    ``exact`` enumerates all subsets (ties keep the subset found first in
    increasing bit-mask order, so fewer and earlier offers); otherwise
    backward elimination from the full set (ties by lowest index)."""
    n = len(candidates)
    nodes = 0
    if exact:
        best: tuple[int, ...] = ()
        best_value, used = value(())
        nodes += used
        for mask in range(1, 1 << n):
            chosen = tuple(i for i in range(n) if mask >> i & 1)
            offers = tuple(candidates[i] for i in chosen)
            if not feasible(offers):
                continue
            current, used = value(offers)
            nodes += used
            if current > best_value:
                best, best_value = chosen, current
        return best, best_value, nodes
    chosen = tuple(range(n))
    current, used = value(tuple(candidates[i] for i in chosen))
    nodes += used
    while chosen:
        removals: list[tuple[int, int]] = []
        for position in range(len(chosen)):
            rest = chosen[:position] + chosen[position + 1 :]
            rest_value, used = value(tuple(candidates[i] for i in rest))
            nodes += used
            removals.append((rest_value, position))
        top_value, top_position = max(removals, key=lambda row: (row[0], -row[1]))
        if top_value > current or not feasible(tuple(candidates[i] for i in chosen)):
            chosen = chosen[:top_position] + chosen[top_position + 1 :]
            current = top_value
            continue
        break
    if not chosen:
        current, used = value(())
        nodes += used
    return chosen, current, nodes


class BotticelliV41Kernel(BotticelliV4Kernel):
    """v4 whose supplier offers are the plan-value-maximising feasible set."""

    strategy_id = "botticelli-scenario-v4-1"

    def __init__(
        self,
        *,
        parameters: BotticelliV3Parameters | None = None,
        model: BotticelliV4Model | None = None,
    ) -> None:
        super().__init__(parameters=parameters, model=model)
        self.strategy_id = "botticelli-scenario-v4-1"

    def _supplier_orders(  # type: ignore[override]
        self, observation: DayObservation, history: PublicHistoryProtocol, mode: str
    ) -> tuple[tuple[SupplierOrderCommand, ...], int, int]:
        if mode == "greedy" or observation.day >= 210:
            return super()._supplier_orders(observation, history, mode)
        candidates = candidate_offers(observation)
        if not candidates:
            return (), 0, 0
        p = self.parameters
        scenarios = _scenarios(mode)
        committed = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        position = _position(observation)

        def value(offers: tuple[SupplierOffer, ...]) -> tuple[int, int]:
            plan, used, _ = _plan_value(observation, history, scenarios, offers)
            return plan, used

        def feasible(offers: tuple[SupplierOffer, ...]) -> bool:
            units = sum(offer.quantity for offer in offers)
            cost = sum(offer.quantity * offer.unit_price for offer in offers)
            return position + units <= p.maximum_position and observation.cash_microcredits - committed - cost >= p.debt_floor

        exact = (1 << len(candidates)) * max(1, len(scenarios)) <= p.operation_budget
        # The plan value reads the module forecast, a pure function of the
        # history (v3's decide sets it); evaluate it once for all subsets.
        forecast_products = v1._forecast_products
        forecast = forecast_products(history)
        v1._forecast_products = lambda _history: dict(forecast)
        try:
            chosen, chosen_value, nodes = select_offers(candidates, value, feasible, exact)
            empty_value, used = value(())
        finally:
            v1._forecast_products = forecast_products
        nodes += used
        selected = tuple(
            SupplierOrderCommand(f"so-{observation.day}-{k}", candidates[i].offer_id) for k, i in enumerate(chosen)
        )
        return selected, chosen_value - empty_value, nodes


__all__ = [
    "BotticelliV41Kernel",
    "candidate_offers",
    "parameters_from_environment",
    "parse_parameters",
    "select_offers",
]
