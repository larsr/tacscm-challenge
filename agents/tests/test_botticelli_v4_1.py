"""Botticelli v4-1 keeps the jointly valuable supplier offers that v4 refuses."""

from __future__ import annotations

import hashlib
import unittest

from tac_scm_sdk.contracts import CustomerRfq, DayObservation, InventoryItem, SupplierOffer, SupplierRfq
from tac_scm_sdk.profile import BASE_MICROCREDITS

from tac_scm_gallery.botticelli_v3 import BotticelliV3Parameters
from tac_scm_gallery.botticelli_v4 import BotticelliV4Kernel
from tac_scm_gallery.botticelli_v4_1 import BotticelliV41Kernel, select_offers
from tac_scm_gallery.history import PublicHistory

DAY = 40


def offer(index: int, component: str, quantity: int, unit_price: int) -> tuple[SupplierRfq, SupplierOffer]:
    supplier = {"cpu-pintel-2": "pintel", "disk-500": "mintor"}[component]
    request = SupplierRfq(
        f"supplier-rfq/{index}", "agent-a", f"bo-{component}-{DAY:03d}{index:03d}", supplier, component,
        quantity, DAY + 5, BASE_MICROCREDITS[component] * 11 // 10,
    )
    return request, SupplierOffer(
        f"supplier-offer/{index}", f"alternative-group/{index}", request.rfq_id, "agent-a", supplier,
        component, "on-date", quantity, DAY + 5, unit_price, valid_day=DAY, reputation_exposure=0, no_offer_reason=None,
    )


def day(with_harmful: bool) -> DayObservation:
    """p01 demand of 20 units a day (200 over the ten-day horizon), every p01
    component but its CPU in stock, and three identical 120-unit CPU offers
    at 600 (base 1000): two offers are worth most, and with all three each
    is redundant given the other two. The optional disk-500 offer has no
    demand at all."""
    rows = [offer(i, "cpu-pintel-2", 120, 600_000_000) for i in range(3)]
    if with_harmful:
        rows.append(offer(3, "disk-500", 50, 400_000_000))
    return DayObservation(
        agent_id="agent-a",
        observation_id=f"observation/{DAY}/agent-a",
        day=DAY,
        state_commitment=hashlib.sha256(b"state").digest(),
        cash_microcredits=0,
        event_cursor=DAY * 100,
        component_inventory=tuple(
            InventoryItem(f"inventory-item/{c}", "agent-a", "component", c, 300)
            for c in ("board-pintel", "memory-1", "disk-300")
        ),
        customer_rfqs=tuple(
            CustomerRfq(f"customer-rfq/{DAY}/high/{i}", "p01", 10, DAY + 7, 3_500_000_000, 100_000_000) for i in range(4)
        ),
        supplier_rfqs=tuple(r for r, _ in rows),
        supplier_offers=tuple(o for _, o in rows),
    )


def orders(kernel, value: DayObservation) -> tuple[str, ...]:  # type: ignore[no-untyped-def]
    history, features = PublicHistory().observe(value)
    decision = kernel.decide(value, features, history)
    return tuple(row.offer_id for row in decision.action.supplier_orders)


class SelectionTests(unittest.TestCase):
    def test_v4_refuses_all_and_v4_1_keeps_the_valuable_pair(self) -> None:
        value = day(with_harmful=False)
        self.assertEqual(orders(BotticelliV4Kernel(), value), ())
        self.assertEqual(len(orders(BotticelliV41Kernel(), value)), 2)

    def test_harmful_offer_is_dropped_exactly_and_by_elimination(self) -> None:
        value = day(with_harmful=True)
        exact = orders(BotticelliV41Kernel(), value)
        # A budget of 40 covers the planner itself but not the 64 nodes of
        # exact enumeration, so this runs backward elimination.
        eliminated = orders(BotticelliV41Kernel(parameters=BotticelliV3Parameters(operation_budget=40)), value)
        for kept in (exact, eliminated):
            self.assertEqual(len(kept), 2)
            self.assertNotIn("supplier-offer/3", kept)

    def test_selection_respects_limits(self) -> None:
        def value(items):  # type: ignore[no-untyped-def]
            keys = set(items)
            return 10 * len(keys) - 15 * ({0, 1} <= keys) - 15 * ({2, 3} <= keys), 1

        def feasible(items):  # type: ignore[no-untyped-def]
            return len(items) <= 2

        for exact in (True, False):
            chosen, best, _ = select_offers((0, 1, 2, 3), value, feasible, exact)  # type: ignore[arg-type]
            self.assertEqual((len(chosen), best), (2, 20))


if __name__ == "__main__":
    unittest.main()
