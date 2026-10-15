"""Deterministic feasibility projection for newly authored strategy kernels."""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from typing import Protocol

from tac_scm_sdk.contracts import DayActionSet, DayObservation
from tac_scm_sdk.profile import PRODUCTS, SUPPLIERS


@dataclass(frozen=True, slots=True)
class SafetyProjection:
    action: DayActionSet
    reasons: tuple[str, ...]


class _InventoryRecord(Protocol):
    item_id: str
    quantity: int


def _inventory(records: Iterable[_InventoryRecord]) -> dict[str, int]:
    result: dict[str, int] = {}
    for record in records:
        result[record.item_id] = result.get(record.item_id, 0) + record.quantity
    return result


def project_safe_action(
    observation: DayObservation, proposed: DayActionSet
) -> SafetyProjection:
    """Drop publicly provable infeasible commands and report every projection."""

    if (
        proposed.agent_id != observation.agent_id
        or proposed.observation_id != observation.observation_id
        or proposed.day != observation.day
    ):
        raise ValueError("proposed action does not bind the current observation")
    reasons: list[str] = []

    customer_rfqs = {rfq.rfq_id: rfq for rfq in observation.customer_rfqs}
    customer_offers = []
    for offer in proposed.customer_offers:
        rfq = customer_rfqs.get(offer.rfq_id)
        if rfq is None or not 0 <= offer.unit_price <= rfq.unit_reserve:
            reasons.append(f"drop-customer-offer:{offer.command_id}")
            continue
        customer_offers.append(offer)

    supplier_rfqs = []
    for request in proposed.supplier_rfqs:
        if (
            request.component_id not in SUPPLIERS
            or request.supplier_id not in SUPPLIERS[request.component_id]
            or request.due_day < observation.day
        ):
            reasons.append(f"drop-supplier-rfq:{request.command_id}")
            continue
        supplier_rfqs.append(request)

    supplier_offers = {
        offer.offer_id: offer
        for offer in observation.supplier_offers
        if offer.kind not in {"none", "probe"}
        and offer.quantity > 0
        and offer.due_day >= observation.day
    }
    accepted_offer_ids: set[str] = set()
    supplier_orders = []
    for command in proposed.supplier_orders:
        if command.offer_id not in supplier_offers or command.offer_id in accepted_offer_ids:
            reasons.append(f"drop-supplier-order:{command.command_id}")
            continue
        accepted_offer_ids.add(command.offer_id)
        supplier_orders.append(command)

    component_stock = _inventory(observation.component_inventory)
    cycles = 2_000
    production = []
    for production_line in sorted(
        proposed.production_schedule, key=lambda item: item.priority
    ):
        product = PRODUCTS.get(production_line.product_id)
        if product is None:
            reasons.append(f"drop-production:{production_line.line_id}")
            continue
        components, product_cycles = product
        if production_line.quantity * product_cycles > cycles or any(
            component_stock.get(component, 0) < production_line.quantity
            for component in components
        ):
            reasons.append(f"drop-production:{production_line.line_id}")
            continue
        for component in components:
            component_stock[component] -= production_line.quantity
        cycles -= production_line.quantity * product_cycles
        production.append(production_line)

    pc_stock = _inventory(observation.pc_inventory)
    open_orders = {
        order.order_id: order
        for order in observation.active_customer_orders
        if order.status == "open"
    }
    delivered_orders: set[str] = set()
    deliveries = []
    for delivery_line in sorted(
        proposed.delivery_schedule, key=lambda item: item.priority
    ):
        order = open_orders.get(delivery_line.customer_order_id)
        if (
            order is None
            or order.order_id in delivered_orders
            or delivery_line.quantity != order.quantity
            or pc_stock.get(order.product_id, 0) < delivery_line.quantity
        ):
            reasons.append(f"drop-delivery:{delivery_line.line_id}")
            continue
        delivered_orders.add(order.order_id)
        pc_stock[order.product_id] -= delivery_line.quantity
        deliveries.append(delivery_line)

    action = DayActionSet(
        observation.agent_id,
        observation.observation_id,
        observation.day,
        proposed.revision,
        tuple(customer_offers),
        tuple(supplier_rfqs),
        tuple(supplier_orders),
        tuple(production),
        tuple(deliveries),
    )
    return SafetyProjection(action, tuple(reasons))
