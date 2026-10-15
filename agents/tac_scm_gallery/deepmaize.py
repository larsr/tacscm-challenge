"""Bounded DeepMaize-inspired value feedback over public observations only."""

from __future__ import annotations

from dataclasses import dataclass

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    CustomerOrder,
    CustomerRfq,
    DayActionSet,
    DayObservation,
    DeliveryLine,
    InventoryItem,
    ProductionLine,
    SupplierOffer,
    SupplierOrderCommand,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS

from .contracts import (
    DecisionExplanation,
    FeatureVector,
    KernelDecision,
    PublicHistoryProtocol,
)


MAX_OPEN_UNITS = 20


@dataclass(frozen=True, slots=True)
class FeedbackSignal:
    component_id: str
    target_units: int
    supply_units: int
    error_units: int
    trend_units: int
    output_units: int
    downstream_value: int
    due_day: int


@dataclass(frozen=True, slots=True)
class FeedbackPlan:
    selected_rfq_id: str | None
    selected_product_id: str | None
    selected_quantity: int
    selected_price: int
    selected_due_day: int
    product_values: tuple[tuple[str, int], ...]
    signals: tuple[FeedbackSignal, ...]
    value_feedback: bool


def _inventory(records: tuple[InventoryItem, ...]) -> dict[str, int]:
    result: dict[str, int] = {}
    for record in records:
        result[record.item_id] = result.get(record.item_id, 0) + record.quantity
    return result


def _nominal_product(product_id: str) -> int:
    return sum(BASE_MICROCREDITS[item] for item in PRODUCTS[product_id][0])


def _client_rfq_id(day: int, product_id: str, index: int) -> str:
    return f"dm-{day:03d}-{product_id}-{index}"


def _parse_client_rfq_id(value: str) -> tuple[int, str] | None:
    fields = value.split("-")
    if len(fields) != 4 or fields[0] != "dm" or not fields[1].isdigit():
        return None
    day = int(fields[1])
    product_id = fields[2]
    if not 0 <= day <= 219 or product_id not in PRODUCTS or not fields[3].isdigit():
        return None
    return day, product_id


def _customer_rfq_day(value: str) -> int | None:
    fields = value.split("/")
    if len(fields) < 2 or fields[0] != "customer-rfq" or not fields[1].isdigit():
        return None
    day = int(fields[1])
    return day if 0 <= day <= 219 else None


def _open_orders(observation: DayObservation) -> tuple[CustomerOrder, ...]:
    return tuple(
        sorted(
            (order for order in observation.active_customer_orders if order.status == "open"),
            key=lambda order: (order.due_day, order.order_id),
        )
    )


def _supply_position(observation: DayObservation) -> dict[str, int]:
    supply = _inventory(observation.component_inventory)
    for item in observation.pending_inventory:
        if item.item_id in SUPPLIERS:
            supply[item.item_id] = supply.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        supply[order.component_id] = supply.get(order.component_id, 0) + max(
            0, order.quantity - order.delivered_quantity
        )
    return supply


def _recent_price(observation: DayObservation, product_id: str) -> int:
    values: list[int] = []
    if observation.market_report is not None:
        for row in observation.market_report.products:
            if row.product_id == product_id and row.mean_price is not None:
                values.append(row.mean_price)
    if observation.daily_price_report is not None:
        for price_range in observation.daily_price_report.products:
            if price_range.product_id == product_id:
                if price_range.minimum is not None:
                    values.append(price_range.minimum)
                if price_range.maximum is not None:
                    values.append(price_range.maximum)
    return min(values) if values else _nominal_product(product_id) * 6 // 5


def _requirements(
    orders: tuple[CustomerOrder, ...],
    pcs: dict[str, int],
    extra: tuple[str, int, int] | None,
) -> tuple[dict[str, int], dict[str, int]]:
    demand = {component: 0 for component in SUPPLIERS}
    due = {component: 219 for component in SUPPLIERS}
    for order in orders:
        covered = min(pcs.get(order.product_id, 0), order.quantity)
        pcs[order.product_id] = pcs.get(order.product_id, 0) - covered
        required = order.quantity - covered
        for component in PRODUCTS[order.product_id][0]:
            demand[component] += required
            due[component] = min(due[component], order.due_day)
    if extra is not None:
        product_id, quantity, due_day = extra
        covered = min(pcs.get(product_id, 0), quantity)
        required = quantity - covered
        for component in PRODUCTS[product_id][0]:
            demand[component] += required
            due[component] = min(due[component], due_day)
    return demand, due


def _product_values(
    observation: DayObservation,
    orders: tuple[CustomerOrder, ...],
) -> dict[str, int]:
    values = {
        product_id: _recent_price(observation, product_id)
        for product_id in PRODUCTS
    }
    for order in orders:
        urgency = max(0, 5 - (order.due_day - observation.day))
        values[order.product_id] = max(
            values[order.product_id],
            order.unit_price + urgency * order.daily_penalty // max(1, order.quantity),
        )
    return values


def _select_rfq(
    observation: DayObservation,
    orders: tuple[CustomerOrder, ...],
    product_values: dict[str, int],
) -> tuple[CustomerRfq, int] | None:
    if orders:
        return None
    open_units = sum(order.quantity for order in orders)
    candidates: list[tuple[int, int, str, CustomerRfq, int]] = []
    for rfq in observation.customer_rfqs:
        lead_days = rfq.due_day - observation.day
        if (
            not 8 <= lead_days <= 14
            or open_units + rfq.quantity > MAX_OPEN_UNITS
        ):
            continue
        nominal = _nominal_product(rfq.product_id)
        minimum = nominal * 11 // 10
        price = rfq.unit_reserve
        surplus = (price - nominal) * rfq.quantity
        if price >= minimum and surplus > 0:
            candidates.append((-surplus, rfq.due_day, rfq.rfq_id, rfq, price))
    if not candidates:
        return None
    selected = min(candidates)
    return selected[3], selected[4]


def build_feedback_plan(
    observation: DayObservation,
    previous: DayObservation | None,
    *,
    value_feedback: bool,
) -> FeedbackPlan:
    """Propagate public product value upstream and close observed target error."""

    orders = _open_orders(observation)
    product_values = _product_values(observation, orders)
    selected = _select_rfq(observation, orders, product_values)
    extra = None if selected is None else (
        selected[0].product_id,
        selected[0].quantity,
        selected[0].due_day,
    )
    demand, due = _requirements(orders, _inventory(observation.pc_inventory), extra)
    supply = _supply_position(observation)
    previous_error = {component: 0 for component in SUPPLIERS}
    if previous is not None:
        previous_orders = _open_orders(previous)
        previous_demand, _ = _requirements(
            previous_orders, _inventory(previous.pc_inventory), None
        )
        previous_supply = _supply_position(previous)
        previous_error = {
            component: max(
                0, previous_demand[component] - previous_supply.get(component, 0)
            )
            for component in SUPPLIERS
        }
    component_values = dict(BASE_MICROCREDITS)
    if value_feedback:
        for product_id, value in product_values.items():
            nominal = _nominal_product(product_id)
            for component in PRODUCTS[product_id][0]:
                propagated = value * BASE_MICROCREDITS[component] // nominal
                component_values[component] = max(
                    component_values[component], propagated
                )
    signals = []
    for component in sorted(SUPPLIERS):
        error = max(0, demand[component] - supply.get(component, 0))
        trend = error - previous_error[component]
        output = min(32, max(0, error + max(-4, min(4, trend)) // 4))
        signals.append(
            FeedbackSignal(
                component,
                demand[component],
                supply.get(component, 0),
                error,
                trend,
                output,
                component_values[component],
                due[component],
            )
        )
    return FeedbackPlan(
        None if selected is None else selected[0].rfq_id,
        None if selected is None else selected[0].product_id,
        0 if selected is None else selected[0].quantity,
        0 if selected is None else selected[1],
        219 if selected is None else selected[0].due_day,
        tuple(sorted(product_values.items())),
        tuple(signals),
        value_feedback,
    )


class DeepMaizeFeedbackKernel:
    """Publication-inspired finite-horizon value feedback controller."""

    strategy_id = "deepmaize-feedback-v1"
    DEBT_FLOOR = -100_000_000_000
    MAX_POSITION = 256

    def __init__(self, *, value_feedback: bool = True) -> None:
        self.value_feedback = value_feedback
        if not value_feedback:
            self.strategy_id = "deepmaize-no-value-v1"

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: PublicHistoryProtocol,
    ) -> KernelDecision:
        del features
        observations = history.observations
        previous = observations[-2] if len(observations) >= 2 else None
        if observation.day == 219:
            return KernelDecision(
                empty_action(observation),
                DecisionExplanation(
                    self.strategy_id, "terminal", "No commitments on the final day.", ("endgame",)
                ),
            )
        plan = build_feedback_plan(
            observation, previous, value_feedback=self.value_feedback
        )
        orders = _open_orders(observation)
        deliveries = self._deliveries(observation, orders)
        production = self._production(observation, orders)
        supplier_orders = self._supplier_orders(observation, orders, plan)
        offers, requests = self._new_business(observation, orders, plan)
        action = DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            offers,
            requests,
            supplier_orders,
            production,
            deliveries,
        )
        errors = sum(signal.error_units for signal in plan.signals)
        output = sum(signal.output_units for signal in plan.signals)
        saturated = sum(
            signal.output_units == 32 and signal.error_units > 32
            for signal in plan.signals
        )
        propagated = sum(signal.downstream_value for signal in plan.signals)
        return KernelDecision(
            action,
            DecisionExplanation(
                self.strategy_id,
                "value-feedback" if self.value_feedback else "nominal-ablation",
                "Close dated component errors using bounded downstream-value feedback.",
                ("reference-trajectory", "downstream-value", "complete-bom"),
                (
                    ("target_error_units", errors),
                    ("controller_output_units", output),
                    ("saturated_components", saturated),
                    ("downstream_value_microcredits", propagated),
                    ("customer_offers", len(offers)),
                    ("supplier_orders", len(supplier_orders)),
                ),
            ),
        )

    def _new_business(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        plan: FeedbackPlan,
    ) -> tuple[tuple[CustomerOfferCommand, ...], tuple[SupplierRfqCommand, ...]]:
        position = sum(
            item.quantity
            for item in observation.component_inventory
            + observation.pc_inventory
            + observation.pending_inventory
        ) + sum(
            max(0, order.quantity - order.delivered_quantity)
            for order in observation.supplier_orders
        )
        if (
            observation.day > 200
            or observation.cash_microcredits <= self.DEBT_FLOOR
            or position >= self.MAX_POSITION
        ):
            return (), ()
        offers: tuple[CustomerOfferCommand, ...] = ()
        if plan.selected_rfq_id is not None:
            offers = (
                CustomerOfferCommand(
                    f"co-{observation.day}-0",
                    plan.selected_rfq_id,
                    plan.selected_price,
                ),
            )
        if observation.day > 199:
            return offers, ()
        requests: list[SupplierRfqCommand] = []
        supply = _supply_position(observation)
        intents: list[tuple[int, str, int, int]] = []
        if orders:
            target = orders[0]
            available_pc = _inventory(observation.pc_inventory).get(
                target.product_id, 0
            )
            target_day = _customer_rfq_day(target.rfq_id)
            target_quantity = max(0, target.quantity - available_pc)
            if target_day is not None and target_quantity:
                intents.append(
                    (
                        target_day,
                        target.product_id,
                        target_quantity,
                        target.due_day,
                    )
                )
        if plan.selected_product_id is not None:
            intents.append(
                (
                    observation.day,
                    plan.selected_product_id,
                    plan.selected_quantity,
                    plan.selected_due_day,
                )
            )
        if not intents:
            return offers, ()
        signals = {signal.component_id: signal for signal in plan.signals}
        for target_day, target_product, target_quantity, target_due_day in intents:
            for component in PRODUCTS[target_product][0]:
                signal = signals[component]
                shortage = max(0, target_quantity - supply.get(component, 0))
                quantity = min(signal.output_units, shortage)
                if quantity == 0:
                    continue
                if (
                    position
                    + sum(item.quantity for item in requests)
                    + quantity
                    > self.MAX_POSITION
                ):
                    continue
                cap = min(
                    BASE_MICROCREDITS[component] * 2,
                    signal.downstream_value * 19 // 20,
                )
                requests.append(
                    SupplierRfqCommand(
                        f"sr-{observation.day}-{len(requests)}",
                        _client_rfq_id(
                            target_day,
                            target_product,
                            len(requests),
                        ),
                        min(SUPPLIERS[component]),
                        component,
                        quantity,
                        max(observation.day, target_due_day - 2),
                        cap,
                    )
                )
                if len(requests) == 8:
                    return offers, tuple(requests)
        return offers, tuple(requests[:8])

    def _supplier_orders(
        self,
        observation: DayObservation,
        orders: tuple[CustomerOrder, ...],
        plan: FeedbackPlan,
    ) -> tuple[SupplierOrderCommand, ...]:
        if not orders or observation.day > 212:
            return ()
        request_by_id = {
            request.rfq_id: request for request in observation.supplier_rfqs
        }
        offer_groups: dict[tuple[int, str], list[SupplierOffer]] = {}
        for offer in observation.supplier_offers:
            request = request_by_id.get(offer.rfq_id)
            if request is None or offer.kind in {"none", "probe"}:
                continue
            intent = _parse_client_rfq_id(request.client_rfq_id)
            if intent is not None and offer.quantity > 0:
                offer_groups.setdefault(intent, []).append(offer)

        pcs = _inventory(observation.pc_inventory)
        demand = {component: 0 for component in SUPPLIERS}
        for order in orders:
            required = max(0, order.quantity - pcs.get(order.product_id, 0))
            pcs[order.product_id] = max(
                0, pcs.get(order.product_id, 0) - order.quantity
            )
            for component in PRODUCTS[order.product_id][0]:
                demand[component] += required
        supply = _supply_position(observation)
        outstanding = sum(
            max(0, order.quantity - order.delivered_quantity) * order.unit_price
            for order in observation.supplier_orders
        )
        values = {
            signal.component_id: signal.downstream_value for signal in plan.signals
        }
        selected_commands: list[SupplierOrderCommand] = []
        spend = 0
        for (bid_day, product_id), offers in sorted(offer_groups.items()):
            matching = [
                order
                for order in orders
                if order.product_id == product_id
                and order.rfq_id.startswith(f"customer-rfq/{bid_day}/")
            ]
            if len(matching) != 1:
                continue
            order = matching[0]
            chosen: list[SupplierOffer] = []
            valued_cost = 0
            complete = True
            for component in PRODUCTS[product_id][0]:
                shortage = max(0, demand[component] - supply.get(component, 0))
                if shortage == 0:
                    valued_cost += order.quantity * BASE_MICROCREDITS[component]
                    continue
                candidates = [
                    offer
                    for offer in offers
                    if offer.component_id == component
                    and shortage <= offer.quantity <= shortage + max(2, shortage // 4)
                    and offer.due_day <= order.due_day - 2
                    and offer.unit_price <= values[component] * 19 // 20
                ]
                if not candidates:
                    complete = False
                    break
                offer = min(
                    candidates,
                    key=lambda item: (
                        item.quantity - shortage,
                        item.unit_price,
                        item.due_day,
                        item.offer_id,
                    ),
                )
                chosen.append(offer)
                valued_cost += order.quantity * offer.unit_price
            new_cost = sum(offer.quantity * offer.unit_price for offer in chosen)
            maximum_cost = order.quantity * order.unit_price * 4 // 5
            if (
                not complete
                or valued_cost > maximum_cost
                or observation.cash_microcredits
                - outstanding
                - spend
                - new_cost
                < self.DEBT_FLOOR
            ):
                continue
            for offer in chosen:
                selected_commands.append(
                    SupplierOrderCommand(
                        f"so-{observation.day}-{len(selected_commands)}",
                        offer.offer_id,
                    )
                )
                supply[offer.component_id] = (
                    supply.get(offer.component_id, 0) + offer.quantity
                )
                spend += offer.quantity * offer.unit_price
        return tuple(selected_commands)

    @staticmethod
    def _production(
        observation: DayObservation, orders: tuple[CustomerOrder, ...]
    ) -> tuple[ProductionLine, ...]:
        components = _inventory(observation.component_inventory)
        pcs = _inventory(observation.pc_inventory)
        cycles = 2_000
        lines: list[ProductionLine] = []
        for order in orders:
            required = max(0, order.quantity - pcs.get(order.product_id, 0))
            pcs[order.product_id] = max(0, pcs.get(order.product_id, 0) - order.quantity)
            bom, unit_cycles = PRODUCTS[order.product_id]
            feasible = min(
                [required, cycles // unit_cycles]
                + [components.get(component, 0) for component in bom]
            )
            if required <= 0 or feasible != required:
                continue
            for component in bom:
                components[component] -= required
            cycles -= required * unit_cycles
            lines.append(
                ProductionLine(
                    f"pl-{observation.day}-{len(lines)}",
                    len(lines),
                    order.product_id,
                    required,
                )
            )
        return tuple(lines)

    @staticmethod
    def _deliveries(
        observation: DayObservation, orders: tuple[CustomerOrder, ...]
    ) -> tuple[DeliveryLine, ...]:
        pcs = _inventory(observation.pc_inventory)
        lines: list[DeliveryLine] = []
        for order in orders:
            if pcs.get(order.product_id, 0) < order.quantity:
                continue
            pcs[order.product_id] -= order.quantity
            lines.append(
                DeliveryLine(
                    f"dl-{observation.day}-{len(lines)}",
                    len(lines),
                    order.order_id,
                    order.quantity,
                )
            )
        return tuple(lines)
