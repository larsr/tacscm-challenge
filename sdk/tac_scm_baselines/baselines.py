# pyright: reportCallIssue=false
"""Transparent baseline-v2 policies with defensive open-order filtering."""

from __future__ import annotations

import hashlib
from collections.abc import Callable, Iterable
from typing import Any

from tac_scm_sdk.contracts import (
    CustomerOfferCommand,
    CustomerOrder,
    DayActionSet,
    DayObservation,
    DeliveryLine,
    ProductionLine,
    SupplierOrderCommand,
    SupplierRfqCommand,
    empty_action,
)
from tac_scm_sdk.profile import BASE_MICROCREDITS, PRODUCTS, SUPPLIERS


CUSTOMER_OFFER_LIMIT = 1
SUPPLIER_RFQ_LIMIT = 1
SUPPLIER_ORDER_LIMIT = 2
PRODUCTION_LINE_LIMIT = 4
DELIVERY_LINE_LIMIT = 4


class HashRandom:
    """Counter-mode SHA-256 stream with unbiased bounded integer sampling."""

    def __init__(self, seed: bytes) -> None:
        self.seed = seed
        self.counter = 0

    def u64(self) -> int:
        raw = hashlib.sha256(self.seed + self.counter.to_bytes(8, "big")).digest()[:8]
        self.counter += 1
        return int.from_bytes(raw, "big")

    def uniform(self, low: int, high: int) -> int:
        width = high - low + 1
        cutoff = ((1 << 64) // width) * width
        while True:
            value = self.u64()
            if value < cutoff:
                return low + value % width


def derive_agent_seed(
    internal_seed: bytes,
    *,
    campaign_id: str,
    artifact_digest: str,
    instance_id: str,
    block_index: int,
    day: int,
) -> bytes:
    """Derive isolated strategy randomness without game-root material."""

    fields = (campaign_id, artifact_digest, instance_id, str(block_index), str(day))
    return hashlib.sha256(
        internal_seed + b"\0" + b"\0".join(field.encode("ascii") for field in fields)
    ).digest()


def open_customer_orders(observation: DayObservation) -> tuple[CustomerOrder, ...]:
    """Return only actionable customer orders, preserving observation order."""

    return tuple(order for order in observation.active_customer_orders if order.status == "open")


def _inventory(observation: DayObservation) -> dict[str, int]:
    result: dict[str, int] = {}
    for item in observation.component_inventory + observation.pc_inventory:
        result[item.item_id] = result.get(item.item_id, 0) + item.quantity
    return result


def _supply_position(observation: DayObservation) -> dict[str, int]:
    result = _inventory(observation)
    for item in observation.pending_inventory:
        result[item.item_id] = result.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        remaining = max(0, order.quantity - order.delivered_quantity)
        result[order.component_id] = result.get(order.component_id, 0) + remaining
    return result


def _supplier_orders(
    observation: DayObservation,
    choose: Callable[[list[Any]], Any] | None = None,
) -> tuple[SupplierOrderCommand, ...]:
    groups: dict[str, list[Any]] = {}
    for offer in observation.supplier_offers:
        if offer.kind != "none" and offer.quantity > 0 and offer.due_day >= observation.day:
            groups.setdefault(offer.alternative_group_id, []).append(offer)
    selected: list[SupplierOrderCommand] = []
    ordered_groups = sorted(groups.values(), key=lambda offers: offers[0].alternative_group_id)
    for index, group in enumerate(ordered_groups[:SUPPLIER_ORDER_LIMIT]):
        ordered = sorted(group, key=lambda offer: (offer.unit_price, offer.due_day, offer.offer_id))
        offer = choose(ordered) if choose else ordered[0]
        selected.append(SupplierOrderCommand(f"so-{observation.day}-{index}", offer.offer_id))
    return tuple(selected)


def _delivery_lines(
    observation: DayObservation,
    orders: Iterable[CustomerOrder],
    random: HashRandom | None = None,
) -> tuple[DeliveryLine, ...]:
    """Schedule inventory only for open orders, even if a caller passes more."""

    stock = _inventory(observation)
    lines: list[DeliveryLine] = []
    actionable = sorted(
        (order for order in orders if order.status == "open"),
        key=lambda order: (order.due_day, order.order_id),
    )
    for order in actionable:
        available = stock.get(order.product_id, 0)
        if available < order.quantity:
            continue
        if random is not None and random.uniform(0, 1) == 0:
            continue
        stock[order.product_id] = available - order.quantity
        lines.append(
            DeliveryLine(
                f"dl-{observation.day}-{len(lines)}",
                len(lines),
                order.order_id,
                order.quantity,
            )
        )
        if len(lines) == DELIVERY_LINE_LIMIT:
            break
    return tuple(lines)


def _production_lines(
    observation: DayObservation,
    preferred: list[str],
    random: HashRandom | None = None,
) -> tuple[ProductionLine, ...]:
    stock = _inventory(observation)
    cycles = 2_000
    lines: list[ProductionLine] = []
    products = preferred + [product for product in sorted(PRODUCTS) if product not in preferred]
    for product_id in products:
        components, product_cycles = PRODUCTS[product_id]
        feasible = min(
            [stock.get(component, 0) for component in components]
            + [cycles // product_cycles, 20]
        )
        if feasible <= 0:
            continue
        quantity = feasible if random is None else random.uniform(1, feasible)
        if random is not None and random.uniform(0, 1) == 0:
            continue
        for component in components:
            stock[component] -= quantity
        cycles -= quantity * product_cycles
        lines.append(
            ProductionLine(
                f"pl-{observation.day}-{len(lines)}",
                len(lines),
                product_id,
                quantity,
            )
        )
        if cycles < 4 or len(lines) >= PRODUCTION_LINE_LIMIT:
            break
    return tuple(lines)


class LegalRandomAgentV2:
    """Uniform bounded choices over a conservative observable legal subset."""

    def __init__(self, seed: bytes, context: tuple[str, str, str, int]) -> None:
        self.seed = seed
        self.context = context

    def act(self, observation: DayObservation) -> DayActionSet:
        if observation.day == 219:
            return empty_action(observation)
        campaign, digest, instance, block = self.context
        random = HashRandom(
            derive_agent_seed(
                self.seed,
                campaign_id=campaign,
                artifact_digest=digest,
                instance_id=instance,
                block_index=block,
                day=observation.day,
            )
        )
        eligible = [rfq for rfq in observation.customer_rfqs if rfq.unit_reserve >= 0]
        ranked = sorted(
            ((random.u64(), rfq.rfq_id, rfq) for rfq in eligible),
            key=lambda item: (item[0], item[1]),
        )
        count = random.uniform(0, min(CUSTOMER_OFFER_LIMIT, len(ranked))) if ranked else 0
        selected = ranked[:count]
        offers = tuple(
            CustomerOfferCommand(
                f"co-{observation.day}-{index}",
                rfq.rfq_id,
                random.uniform(0, rfq.unit_reserve),
            )
            for index, (_, _, rfq) in enumerate(sorted(selected, key=lambda item: item[1]))
        )
        supplier_orders = _supplier_orders(
            observation, lambda choices: choices[random.uniform(0, len(choices) - 1)]
        )
        supply = _supply_position(observation)
        rfqs: list[SupplierRfqCommand] = []
        components = sorted(
            (random.u64(), component)
            for component in SUPPLIERS
            if supply.get(component, 0) < 20
        )
        for _, component in components:
            if len(rfqs) >= SUPPLIER_RFQ_LIMIT:
                continue
            suppliers = SUPPLIERS[component]
            supplier = suppliers[random.uniform(0, len(suppliers) - 1)]
            rfqs.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(rfqs)}",
                    f"r-{observation.day}-{len(rfqs)}",
                    supplier,
                    component,
                    random.uniform(1, 20),
                    min(219, observation.day + random.uniform(3, 12)),
                    None,
                )
            )
        orders = open_customer_orders(observation)
        preferred = [order.product_id for order in orders]
        return DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            offers,
            tuple(rfqs),
            supplier_orders,
            _production_lines(observation, preferred, random),
            _delivery_lines(observation, orders, random),
        )


class HeuristicAgentV2:
    """Fixed-rule inventory, margin, earliest-due, and production policy."""

    TARGET_COMPONENTS = 24
    CUSTOMER_DISCOUNT_PPB = 100_000_000
    RFQ_LEAD_DAYS = 7

    def act(self, observation: DayObservation) -> DayActionSet:
        if observation.day == 219:
            return empty_action(observation)
        offers = tuple(
            CustomerOfferCommand(
                f"co-{observation.day}-{index}",
                rfq.rfq_id,
                rfq.unit_reserve * (1_000_000_000 - self.CUSTOMER_DISCOUNT_PPB)
                // 1_000_000_000,
            )
            for index, rfq in enumerate(
                sorted(observation.customer_rfqs, key=lambda item: item.rfq_id)[
                    :CUSTOMER_OFFER_LIMIT
                ]
            )
        )
        supply = _supply_position(observation)
        rfqs: list[SupplierRfqCommand] = []
        for component in sorted(SUPPLIERS):
            shortage = max(0, self.TARGET_COMPONENTS - supply.get(component, 0))
            if shortage == 0 or len(rfqs) == SUPPLIER_RFQ_LIMIT:
                continue
            suppliers = SUPPLIERS[component]
            supplier = suppliers[observation.day % len(suppliers)]
            rfqs.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(rfqs)}",
                    f"r-{observation.day}-{len(rfqs)}",
                    supplier,
                    component,
                    shortage,
                    min(219, observation.day + self.RFQ_LEAD_DAYS),
                    BASE_MICROCREDITS[component] * 2,
                )
            )
        orders = tuple(
            sorted(open_customer_orders(observation), key=lambda order: (order.due_day, order.order_id))
        )
        preferred = [order.product_id for order in orders]
        return DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            offers,
            tuple(rfqs),
            _supplier_orders(observation),
            _production_lines(observation, preferred),
            _delivery_lines(observation, orders),
        )


class RollingHorizonAgentV2:
    """Deterministic ten-day integer rolling-horizon policy."""

    HORIZON = 10
    TARGET_COMPONENTS = 36
    CUSTOMER_DISCOUNT_PPB = 50_000_000

    def act(self, observation: DayObservation) -> DayActionSet:
        if observation.day == 219:
            return empty_action(observation)
        orders = tuple(
            sorted(
                open_customer_orders(observation),
                key=lambda order: (
                    -(
                        order.unit_price * order.quantity
                        - order.daily_penalty * max(0, observation.day - order.due_day)
                    ),
                    order.due_day,
                    order.order_id,
                ),
            )
        )
        preferred = [
            order.product_id
            for order in orders
            if order.due_day <= observation.day + self.HORIZON
        ]
        offers: list[CustomerOfferCommand] = []
        customer_rfqs = sorted(
            observation.customer_rfqs,
            key=lambda rfq: (-rfq.unit_reserve, rfq.due_day, rfq.rfq_id),
        )[:CUSTOMER_OFFER_LIMIT]
        for index, rfq in enumerate(customer_rfqs):
            nominal = sum(BASE_MICROCREDITS[component] for component in PRODUCTS[rfq.product_id][0])
            bid = max(
                nominal * 11 // 10,
                rfq.unit_reserve * (1_000_000_000 - self.CUSTOMER_DISCOUNT_PPB)
                // 1_000_000_000,
            )
            if bid <= rfq.unit_reserve:
                offers.append(CustomerOfferCommand(f"co-{observation.day}-{index}", rfq.rfq_id, bid))
        demand = {component: self.TARGET_COMPONENTS for component in SUPPLIERS}
        for product_id in preferred:
            for component in PRODUCTS[product_id][0]:
                demand[component] += 1
        supply = _supply_position(observation)
        rfqs: list[SupplierRfqCommand] = []
        ranked_components = sorted(
            demand, key=lambda component: (-(demand[component] - supply.get(component, 0)), component)
        )
        for component in ranked_components:
            shortage = max(0, demand[component] - supply.get(component, 0))
            if shortage == 0 or len(rfqs) == SUPPLIER_RFQ_LIMIT:
                continue
            rfqs.append(
                SupplierRfqCommand(
                    f"sr-{observation.day}-{len(rfqs)}",
                    f"r-{observation.day}-{len(rfqs)}",
                    min(SUPPLIERS[component]),
                    component,
                    shortage,
                    min(219, observation.day + self.HORIZON),
                    BASE_MICROCREDITS[component] * 3 // 2,
                )
            )
        return DayActionSet(
            observation.agent_id,
            observation.observation_id,
            observation.day,
            1,
            tuple(offers),
            tuple(rfqs),
            _supplier_orders(observation),
            _production_lines(observation, preferred),
            _delivery_lines(observation, orders),
        )
