#!/usr/bin/env python3
"""A minimal TAC SCM agent to start from.

Run by the game runner as an executable: it reads framed observations on
stdin and writes one action set per day on stdout (`tac_scm_sdk.runtime.serve`
does the protocol; sdk/PROTOCOL.md describes it). Replace the policy in
`TemplateAgent.act`; everything it needs is in the observation.

The policy is deliberately simple and plays legally:

1. deliver open customer orders from finished stock, earliest due first;
2. assemble computers for open orders from components on hand;
3. bid on today's customer RFQs at 90% of the reserve price while the
   components on hand, net of open orders, cover the quantity;
4. keep about TARGET units of every component in the supply pipeline with
   one RFQ per short component, and accept the cheapest offer of each
   answered RFQ whose price is at most the component's base price.

Try it: `python runner/play_game.py --out runs/t0 sdk/template/my_agent.py
baseline-heuristic baseline-optimizer iota-inspired-v1 narc-inspired-v1
botticelli-inspired-v4`.
"""

from __future__ import annotations

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
from tac_scm_sdk.runtime import serve

LAST_DAY = 219
DAILY_CYCLES = 2_000  # factory capacity per day (rules/PROFILE.md)
TARGET = 30  # components of each type to keep in stock or on order
SUPPLY_LEAD = 6  # days between a supplier RFQ and its requested due day
BID_FRACTION_PPM = 900_000  # bid at 90% of the customer's reserve price
MAX_BIDS = 12


def stock(observation: DayObservation) -> dict[str, int]:
    """Components and finished computers on hand."""

    result: dict[str, int] = {}
    for item in observation.component_inventory + observation.pc_inventory:
        result[item.item_id] = result.get(item.item_id, 0) + item.quantity
    return result


def supply_position(observation: DayObservation) -> dict[str, int]:
    """Stock plus components in transit plus undelivered supplier orders."""

    result = stock(observation)
    for item in observation.pending_inventory:
        result[item.item_id] = result.get(item.item_id, 0) + item.quantity
    for order in observation.supplier_orders:
        result[order.component_id] = result.get(order.component_id, 0) + max(0, order.quantity - order.delivered_quantity)
    return result


class TemplateAgent:
    def act(self, observation: DayObservation) -> DayActionSet:
        day = observation.day
        if day == LAST_DAY:
            return empty_action(observation)
        on_hand = stock(observation)
        open_orders = sorted(
            (order for order in observation.active_customer_orders if order.status == "open"),
            key=lambda order: (order.due_day, order.order_id),
        )

        # 1. Deliveries from finished stock.
        deliveries: list[DeliveryLine] = []
        for order in open_orders:
            if on_hand.get(order.product_id, 0) >= order.quantity:
                on_hand[order.product_id] -= order.quantity
                deliveries.append(DeliveryLine(f"d{day}-{len(deliveries)}", len(deliveries), order.order_id, order.quantity))

        # 2. Production for the open orders not yet covered by finished
        # stock (make to order), earliest due first, within today's cycles.
        cycles = DAILY_CYCLES
        production: list[ProductionLine] = []
        delivered = {line.customer_order_id for line in deliveries}
        for order in open_orders:
            if order.order_id in delivered:
                continue
            components, product_cycles = PRODUCTS[order.product_id]
            missing = max(0, order.quantity - on_hand.get(order.product_id, 0))
            on_hand[order.product_id] = max(0, on_hand.get(order.product_id, 0) - order.quantity)
            quantity = min([missing, cycles // product_cycles] + [on_hand.get(c, 0) for c in components])
            if quantity <= 0:
                continue
            for component in components:
                on_hand[component] -= quantity
            cycles -= quantity * product_cycles
            production.append(ProductionLine(f"p{day}-{len(production)}", len(production), order.product_id, quantity))

        # 3. Customer bids covered by the components left on hand.
        offers: list[CustomerOfferCommand] = []
        for rfq in sorted(observation.customer_rfqs, key=lambda rfq: -rfq.unit_reserve):
            components, _ = PRODUCTS[rfq.product_id]
            if len(offers) == MAX_BIDS or any(on_hand.get(c, 0) < rfq.quantity for c in components):
                continue
            cost = sum(BASE_MICROCREDITS[c] for c in components)
            price = rfq.unit_reserve * BID_FRACTION_PPM // 1_000_000
            if price < cost:
                continue
            for component in components:
                on_hand[component] -= rfq.quantity
            offers.append(CustomerOfferCommand(f"o{day}-{len(offers)}", rfq.rfq_id, price))

        # 4. Supplier RFQs for short components; accept affordable offers.
        rfqs: list[SupplierRfqCommand] = []
        due = min(LAST_DAY, day + SUPPLY_LEAD)
        if due < LAST_DAY - 10:
            position = supply_position(observation)
            for component in sorted(SUPPLIERS):
                shortage = TARGET - position.get(component, 0)
                if shortage > 0:
                    suppliers = SUPPLIERS[component]
                    supplier = suppliers[day % len(suppliers)]
                    rfqs.append(SupplierRfqCommand(
                        f"s{day}-{len(rfqs)}", f"r{day}-{len(rfqs)}", supplier, component,
                        shortage, due, BASE_MICROCREDITS[component],
                    ))
        groups: dict[str, list] = {}
        for offer in observation.supplier_offers:
            if offer.kind != "none" and offer.quantity > 0 and offer.due_day >= day:
                groups.setdefault(offer.alternative_group_id, []).append(offer)
        orders: list[SupplierOrderCommand] = []
        for group_id in sorted(groups):
            best = min(groups[group_id], key=lambda offer: (offer.unit_price, offer.due_day, offer.offer_id))
            if best.unit_price <= BASE_MICROCREDITS[best.component_id]:
                orders.append(SupplierOrderCommand(f"a{day}-{len(orders)}", best.offer_id))

        return DayActionSet(
            observation.agent_id, observation.observation_id, day, 1,
            tuple(offers), tuple(rfqs), tuple(orders), tuple(production), tuple(deliveries),
        )


def main() -> int:
    return serve(TemplateAgent())


if __name__ == "__main__":
    raise SystemExit(main())
