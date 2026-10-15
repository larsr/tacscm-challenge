"""Botticelli-inspired v4: v3 with the published acceptance model.

Benisch et al. 2004, section 5.1: per product (SKU) the modeling module fits a
linear price-to-win-probability cdf by least squares to d + 2 points: the
previous day's minimum order price at probability 1, the previous day's
maximum at 0, and, for each of the last d = 5 days, Botticelli's mean offer
price against its ratio of offers won to offers issued. Sections 3.3 and 5.3:
the bidding problem is solved by the expected-value method with discretized
prices (the best of the three bidders, EB), offers being scheduled by
expected quantity (win probability x quantity).

v4 replaces exactly two v3 rules and keeps everything else of v3:

- v3's price ``max(1.1 x nominal, 0.88 x reserve)`` becomes the price chosen
  from a discretized set by expected profit under the fitted model;
- v3's constant 40% expected win rate in capacity and component cover
  becomes the model's win probability at the chosen price.

Bid cover and capacity follow the published scheduler (sections 2, 3.2 and
5.2): components count only when expected in inventory by the day before
the RFQ's due day, net of open orders' needs, and the cycles of the
production days before the due day are those of the triangle heuristic
(day k of D = 10 offers C (D - k + 1) / D), net of open orders. v3's
undated cover (all confirmed supply) and flat 16,000-cycle budget let v4's
higher win rates over-commit (in one development game: late penalties
8.5tn vs 3.2tn, 734 cancelled orders vs 220).

Expected profit is price minus the agent's own unit cost of the product: per
component, the quantity-weighted price of its newest supplier orders that
make up its current supply (stock plus undelivered), else the base price.
v1/v3's price floor of 1.1 x nominal is a declared 2026 rule, not part of the
published bidder, and is not applied to fitted prices; in strong markets the
previous day's maximum price is often below it.

The selection is the expected-value program's linear relaxation solved
greedily: per RFQ the options (price, expected units, expected profit) form
an upper convex hull over the expected cycles they use; increments of all
RFQs are taken in order of profit per cycle (the paper's greedy scheduler
order) while v3's reserved cycles and component cover allow.

Adaptations to the 2026 game: the engine reports
the same previous-day minimum and maximum, but reports None for a product
that sold nothing; the last reported range of that product is used then.
Until a product has a usable fit (day 0, or no range yet, or a non-negative
slope) v3's price rule is used for it. The number of price points is not
published; ``price_points`` is a declared 2026 choice.
"""

from __future__ import annotations

from dataclasses import dataclass, fields
import json
import os

from tac_scm_sdk.contracts import CustomerOfferCommand, CustomerOrder, DayObservation
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS

from .botticelli import HORIZON_DAYS, _inventory, _nominal_product, _scenarios, _supply
from .botticelli_v3 import FACTORY_CYCLES, BotticelliV3Kernel, BotticelliV3Parameters
from .contracts import PublicHistoryProtocol

PARAMETERS_ENVIRONMENT_VARIABLE = "TAC_SCM_GALLERY_BOTTICELLI_V4_PARAMETERS"
PPM = 1_000_000


@dataclass(frozen=True, slots=True)
class BotticelliV4Model:
    history_days: int = 5  # published d
    price_points: int = 20  # discretization of the price set (not published)

    def __post_init__(self) -> None:
        for field in fields(self):
            value = getattr(self, field.name)
            if type(value) is not int or value <= 0:
                raise ValueError(f"Botticelli v4 parameter {field.name} is out of range")


def parse_parameters(text: str | None) -> tuple[BotticelliV3Parameters, BotticelliV4Model]:
    if text is None or text.strip() == "":
        return BotticelliV3Parameters(), BotticelliV4Model()
    overrides = json.loads(text)
    if type(overrides) is not dict:
        raise ValueError("Botticelli v4 parameters must be a JSON object")
    v3_names = {field.name for field in fields(BotticelliV3Parameters)}
    v4_names = {field.name for field in fields(BotticelliV4Model)}
    unknown = set(overrides) - v3_names - v4_names
    if unknown:
        raise ValueError(f"unknown Botticelli v4 parameters: {sorted(unknown)}")
    return (
        BotticelliV3Parameters(**{k: v for k, v in overrides.items() if k in v3_names}),
        BotticelliV4Model(**{k: v for k, v in overrides.items() if k in v4_names}),
    )


def parameters_from_environment() -> tuple[BotticelliV3Parameters, BotticelliV4Model]:
    return parse_parameters(os.environ.get(PARAMETERS_ENVIRONMENT_VARIABLE))


def fit_linear_cdf(points: list[tuple[float, float]]) -> tuple[float, float] | None:
    """Least-squares line p = a + b x through (price, probability) points;
    None unless it falls with price (b < 0)."""
    if len(points) < 2:
        return None
    n = len(points)
    mean_x = sum(x for x, _ in points) / n
    mean_y = sum(y for _, y in points) / n
    sxx = sum((x - mean_x) ** 2 for x, _ in points)
    if sxx <= 0:
        return None
    slope = sum((x - mean_x) * (y - mean_y) for x, y in points) / sxx
    if slope >= 0:
        return None
    return mean_y - slope * mean_x, slope


def _hull(options: list[tuple[float, float, int]]) -> list[tuple[float, float, int]]:
    """Upper concave hull from (0, 0) of (resource, profit, price) options,
    keeping only steps with positive profit per resource."""
    points = sorted({(r, v, price) for r, v, price in options if r > 0 and v > 0}, key=lambda t: (t[0], -t[1]))
    hull: list[tuple[float, float, int]] = [(0.0, 0.0, 0)]
    for point in points:
        if point[1] <= hull[-1][1]:
            continue
        while len(hull) >= 2:
            (r1, v1, _), (r2, v2, _) = hull[-2], hull[-1]
            # drop hull[-1] if it lies on or below the segment hull[-2] -> point
            if (v2 - v1) * (point[0] - r1) <= (point[1] - v1) * (r2 - r1):
                hull.pop()
            else:
                break
        hull.append(point)
    return hull


class BotticelliV4Kernel(BotticelliV3Kernel):
    """v3 with the published in-game linear acceptance model and EV bidding."""

    strategy_id = "botticelli-scenario-v4"

    def __init__(
        self,
        *,
        parameters: BotticelliV3Parameters | None = None,
        model: BotticelliV4Model | None = None,
    ) -> None:
        super().__init__(parameters=parameters)
        self.strategy_id = "botticelli-scenario-v4"
        self.model = BotticelliV4Model() if model is None else model
        # day -> product -> list of (rfq id, unit price) offered that day
        self._offered: dict[int, dict[str, list[tuple[str, int]]]] = {}
        # Own supplier orders seen so far (order id -> order): under profile
        # draft3 delivered orders leave the observation after one report,
        # and the unit cost reads the newest orders behind the
        # current supply, delivered ones included.
        self._seen_orders: dict[str, object] = {}
        # day -> product -> (mean offer price, won / issued)
        self._points: dict[int, dict[str, tuple[float, float]]] = {}
        self._last_range: dict[str, tuple[int, int]] = {}
        self.fits_used = 0
        self.fallback_bids = 0
        # Diagnostics: TAC_SCM_GALLERY_BOTTICELLI_V4_TRACE=PATH appends one JSON
        # line per day (fits and bids with their predicted win probability).
        self._trace_path = os.environ.get("TAC_SCM_GALLERY_BOTTICELLI_V4_TRACE")

    # -- modeling module ----------------------------------------------------

    def _observe(self, observation: DayObservation) -> None:
        won = {order.rfq_id for order in observation.new_customer_orders}
        for day, by_product in list(self._offered.items()):
            if day >= observation.day:
                continue
            self._points[day] = {
                product: (sum(price for _, price in rows) / len(rows), sum(rfq in won for rfq, _ in rows) / len(rows))
                for product, rows in by_product.items()
                if rows
            }
            del self._offered[day]
        report = observation.daily_price_report
        if report is not None:
            for row in report.products:
                if row.minimum is not None and row.maximum is not None:
                    self._last_range[row.product_id] = (row.minimum, row.maximum)

    def _models(self, day: int) -> dict[str, tuple[float, float]]:
        result: dict[str, tuple[float, float]] = {}
        recent = [self._points[d] for d in range(day - self.model.history_days, day) if d in self._points]
        for product in PRODUCTS:
            span = self._last_range.get(product)
            if span is None:
                continue
            points: list[tuple[float, float]] = [(float(span[0]), 1.0), (float(span[1]), 0.0)]
            points += [row[product] for row in recent if product in row]
            fit = fit_linear_cdf(points)
            if fit is not None:
                result[product] = fit
        return result

    # -- bidding module -----------------------------------------------------

    def _unit_costs(self, observation: DayObservation) -> dict[str, float]:
        """Per component: weighted price of the newest own supplier orders
        that make up the current supply; the base price without orders.
        Orders no longer in the observation come from memory, before the
        observation's own (under draft2 the observation holds them all, so
        the list is exactly the observation's)."""
        supply = _supply(observation)
        costs: dict[str, float] = {}
        filled: dict[str, int] = {}
        spent: dict[str, int] = {}
        current = {order.order_id for order in observation.supplier_orders}
        orders = [order for order_id, order in self._seen_orders.items() if order_id not in current]
        orders += list(observation.supplier_orders)
        for order in observation.supplier_orders:
            self._seen_orders.setdefault(order.order_id, order)
        for order in reversed(orders):
            component = order.component_id
            need = max(1, supply.get(component, 0)) - filled.get(component, 0)
            if need <= 0:
                continue
            take = min(need, order.quantity)
            filled[component] = filled.get(component, 0) + take
            spent[component] = spent.get(component, 0) + take * order.unit_price
        for component, base in BASE_MICROCREDITS.items():
            costs[component] = spent[component] / filled[component] if filled.get(component) else float(base)
        return costs

    def _customer_offers(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        history: PublicHistoryProtocol,
        mode: str,
    ) -> tuple[CustomerOfferCommand, ...]:
        self._observe(observation)
        offers = self._expected_value_offers(observation, orders, mode)
        rfqs = {rfq.rfq_id: rfq for rfq in observation.customer_rfqs}
        by_product: dict[str, list[tuple[str, int]]] = {}
        for offer in offers:
            rfq = rfqs[offer.rfq_id]
            by_product.setdefault(rfq.product_id, []).append((rfq.rfq_id, offer.unit_price))
        self._offered[observation.day] = by_product
        return offers

    def _expected_value_offers(
        self, observation: DayObservation, orders: tuple[CustomerOrder, ...], mode: str
    ) -> tuple[CustomerOfferCommand, ...]:
        p = self.parameters
        day = observation.day
        if day >= 207:
            return ()
        models = self._models(day)
        unit_costs = self._unit_costs(observation)
        scenarios = _scenarios(mode)
        demand_weight = 1.0 if not scenarios else sum(
            s.probability_ppm * s.demand_ppm // PPM for s in scenarios
        ) / PPM
        pcs = _inventory(observation.pc_inventory)
        # Dated component availability (section 2: the quantity of each
        # component expected in inventory on each day) net of open orders,
        # and committed cycles per production day.
        arrivals: dict[str, list[tuple[int, int]]] = {
            c: [(day, q)] for c, q in _inventory(observation.component_inventory).items()
        }
        for item in observation.pending_inventory:
            arrivals.setdefault(item.item_id, []).append((day + 1, item.quantity))
        for supply in observation.supplier_orders:
            remaining = max(0, supply.quantity - supply.delivered_quantity)
            if remaining:
                arrivals.setdefault(supply.component_id, []).append((max(day + 1, supply.due_day), remaining))
        allocated: dict[str, float] = {}
        committed = [0.0] * 14
        for order in orders:
            covered = min(pcs.get(order.product_id, 0), order.quantity)
            pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
            for component in PRODUCTS[order.product_id][0]:
                allocated[component] = allocated.get(component, 0.0) + (order.quantity - covered)
            committed[max(1, min(13, order.due_day - 1 - day))] += (order.quantity - covered) * PRODUCTS[order.product_id][1]
        open_units = sum(order.quantity for order in orders)

        def available(component: str, by_day: int) -> float:
            return sum(q for t, q in arrivals.get(component, ()) if t <= by_day) - allocated.get(component, 0.0)

        # Triangle heuristic (section 5.2): of the D-day schedule, day k
        # offers C (D - k + 1) / D cycles to the scheduler.
        triangle = [0.0] + [FACTORY_CYCLES * max(0, HORIZON_DAYS - k + 1) / HORIZON_DAYS for k in range(1, 14)]
        # Per RFQ: hull of (expected cycles, expected profit, price) options.
        increments: list[tuple[float, int, str, int, float, float]] = []
        chosen_rfqs: dict[str, object] = {}
        for rfq in observation.customer_rfqs:
            if not 4 <= rfq.due_day - day <= 12 or open_units + rfq.quantity > p.maximum_open_units:
                continue
            nominal = _nominal_product(rfq.product_id)
            floor = nominal * 1_100_000 // PPM
            unit_cycles = PRODUCTS[rfq.product_id][1]
            fit = models.get(rfq.product_id)
            options: list[tuple[float, float, int]] = []
            if fit is None:
                # v3's rule and constant 40% expected win rate until a fit exists.
                price = min(rfq.unit_reserve, max(floor, rfq.unit_reserve * 880_000 // PPM))
                if price >= floor and price > nominal:
                    units = rfq.quantity * 0.4
                    options.append((units * unit_cycles, units * (price - nominal) * demand_weight, price))
            else:
                a, b = fit
                cost = sum(unit_costs[c] for c in PRODUCTS[rfq.product_id][0])
                certain = (1.0 - a) / b  # price at probability 1
                never = -a / b  # price at probability 0
                low, high = max(int(cost) + 1, int(certain)), min(rfq.unit_reserve, int(never))
                if high >= low:
                    steps = self.model.price_points
                    for k in range(steps + 1):
                        price = low + (high - low) * k // steps
                        win = min(1.0, max(0.0, a + b * price))
                        units = rfq.quantity * win
                        if units > 0 and price > cost:
                            options.append((units * unit_cycles, units * (price - cost) * demand_weight, price))
            hull = _hull(options)
            for index in range(1, len(hull)):
                r0, v0, _ = hull[index - 1]
                r1, v1, price = hull[index]
                efficiency = (v1 - v0) / (r1 - r0)
                increments.append((-efficiency, index, rfq.rfq_id, price, r1 - r0, v1 - v0))
            chosen_rfqs[rfq.rfq_id] = (rfq, fit is None)
        stock = {product: float(max(0, quantity)) for product, quantity in pcs.items()}
        level: dict[str, int] = {}
        blocked: set[str] = set()
        price_of: dict[str, int] = {}
        offered_order: list[str] = []
        for _, index, rfq_id, price, extra_cycles, _gain in sorted(increments, key=lambda row: (row[0], row[2], row[1])):
            if rfq_id in blocked or level.get(rfq_id, 0) != index - 1:
                continue
            if rfq_id not in price_of and len(offered_order) == p.max_offers_per_day:
                continue
            rfq, _ = chosen_rfqs[rfq_id]  # type: ignore[misc]
            bom, unit_cycles = PRODUCTS[rfq.product_id]
            extra_units = extra_cycles / unit_cycles
            from_stock = min(stock.get(rfq.product_id, 0.0), extra_units)
            required = extra_units - from_stock
            last = max(1, min(13, rfq.due_day - 1 - day))  # last production day offset
            window = range(1, last + 1)
            load = sum(committed[k] for k in window)
            if load + required * unit_cycles > sum(triangle[k] for k in window) or any(
                available(c, rfq.due_day - 1) < required for c in bom
            ):
                blocked.add(rfq_id)
                continue
            for c in bom:
                allocated[c] = allocated.get(c, 0.0) + required
            stock[rfq.product_id] = stock.get(rfq.product_id, 0.0) - from_stock
            committed[last] += required * unit_cycles
            level[rfq_id] = index
            if rfq_id not in price_of:
                offered_order.append(rfq_id)
            price_of[rfq_id] = price
        if self._trace_path:
            with open(self._trace_path, "a") as stream:
                stream.write(json.dumps({
                    "day": day,
                    "fits": {k: v for k, v in models.items()},
                    "bids": [
                        [rfq_id, chosen_rfqs[rfq_id][0].product_id, price_of[rfq_id],  # type: ignore[index]
                         (models[chosen_rfqs[rfq_id][0].product_id][0] + models[chosen_rfqs[rfq_id][0].product_id][1] * price_of[rfq_id])  # type: ignore[index]
                         if chosen_rfqs[rfq_id][0].product_id in models else None,  # type: ignore[index]
                         chosen_rfqs[rfq_id][0].quantity]  # type: ignore[index]
                        for rfq_id in offered_order
                    ],
                    "won": sorted(o.rfq_id for o in observation.new_customer_orders),
                    "committed": committed,
                }) + "\n")
        result: list[CustomerOfferCommand] = []
        for rfq_id in offered_order:
            rfq, fallback = chosen_rfqs[rfq_id]  # type: ignore[misc]
            if fallback:
                self.fallback_bids += 1
            else:
                self.fits_used += 1
            result.append(CustomerOfferCommand(f"co4-{day}-{len(result)}", rfq_id, price_of[rfq_id]))
        return tuple(result)


__all__ = [
    "PARAMETERS_ENVIRONMENT_VARIABLE",
    "BotticelliV4Kernel",
    "BotticelliV4Model",
    "fit_linear_cdf",
    "parameters_from_environment",
    "parse_parameters",
]
