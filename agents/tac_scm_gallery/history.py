"""Agent-local public history and stable feature extraction."""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass, replace
from typing import Protocol

from tac_scm_sdk.contracts import DayObservation

from .contracts import FeatureVector


FEATURE_NAMES = (
    "day",
    "cash_microcredits",
    "cash_change_microcredits",
    "component_inventory_units",
    "pc_inventory_units",
    "pending_inventory_units",
    "open_supplier_units",
    "open_customer_units",
    "urgent_customer_units",
    "overdue_customer_units",
    "customer_rfq_count",
    "customer_rfq_units",
    "supplier_offer_count",
    "supplier_offer_units",
    "supplier_delivery_units",
    "customer_settlement_count",
    "default_count",
    "market_rfq_units",
    "market_order_units",
)


class _QuantityRecord(Protocol):
    quantity: int


def _sum_quantities(records: Iterable[_QuantityRecord]) -> int:
    return sum((record.quantity for record in records), start=0)


def extract_features(
    observation: DayObservation, previous: DayObservation | None
) -> FeatureVector:
    """Extract the v1 integer vector without any authoritative-state access."""

    open_orders = tuple(
        order for order in observation.active_customer_orders if order.status == "open"
    )
    market_rfq_units = 0
    market_order_units = 0
    if observation.market_report is not None:
        market_rfq_units = sum(row.rfq_quantity for row in observation.market_report.products)
        market_order_units = sum(
            row.order_quantity for row in observation.market_report.products
        )
    values = (
        observation.day,
        observation.cash_microcredits,
        0
        if previous is None
        else observation.cash_microcredits - previous.cash_microcredits,
        _sum_quantities(observation.component_inventory),
        _sum_quantities(observation.pc_inventory),
        _sum_quantities(observation.pending_inventory),
        sum(
            max(0, order.quantity - order.delivered_quantity)
            for order in observation.supplier_orders
        ),
        sum(order.quantity for order in open_orders),
        sum(
            order.quantity
            for order in open_orders
            if order.due_day <= observation.day + 3
        ),
        sum(
            order.quantity for order in open_orders if order.due_day < observation.day
        ),
        len(observation.customer_rfqs),
        _sum_quantities(observation.customer_rfqs),
        sum(1 for offer in observation.supplier_offers if offer.kind != "none"),
        sum(
            offer.quantity
            for offer in observation.supplier_offers
            if offer.kind != "none"
        ),
        _sum_quantities(observation.supplier_deliveries),
        len(observation.customer_settlements),
        len(observation.default_history),
        market_rfq_units,
        market_order_units,
    )
    return FeatureVector(observation.day, FEATURE_NAMES, values)


@dataclass(frozen=True, slots=True)
class PublicHistory:
    """Immutable history containing exactly what the ordinary agent observed."""

    observations: tuple[DayObservation, ...] = ()
    features: tuple[FeatureVector, ...] = ()

    def observe(self, observation: DayObservation) -> tuple["PublicHistory", FeatureVector]:
        previous = self.observations[-1] if self.observations else None
        if previous is not None and observation.day <= previous.day:
            raise ValueError("public observations must increase by day")
        features = extract_features(observation, previous)
        return (
            replace(
                self,
                observations=self.observations + (observation,),
                features=self.features + (features,),
            ),
            features,
        )

    @property
    def latest(self) -> DayObservation | None:
        return self.observations[-1] if self.observations else None
