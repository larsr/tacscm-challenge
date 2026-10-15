"""SDK checks: message/2 records, frame limits, the RFQ rule and the template.

Run from sdk/: `python -m unittest discover -s tests`.
"""

from __future__ import annotations

import hashlib
import io
from pathlib import Path
import sys
import unittest

from tac_scm_sdk import ContractError, DayActionSet, DayObservation, TransportError, read_frame, write_frame
from tac_scm_sdk.cbor import dumps
from tac_scm_sdk.contracts import (
    MESSAGE_SCHEMA_ID,
    MESSAGE_SCHEMA_VERSION,
    CustomerOrder,
    CustomerRfq,
    DailyPriceReport,
    InventoryItem,
    PriceRange,
    Message,
    SupplierOffer,
    SupplierOrder,
    SupplierRfqCommand,
    logical_value,
    parse_payload,
)
from tac_scm_sdk.transport import (
    MAX_ACTION_FRAME_BYTES,
    MAX_OBSERVATION_FRAME_BYTES,
    action_message,
    decode_message,
    encode_message,
)

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "template"))
from my_agent import TemplateAgent  # noqa: E402


def observation(day: int = 5, pcs: int = 3) -> DayObservation:
    agent = "agent-a"
    return DayObservation(
        agent_id=agent,
        observation_id=f"observation/{day}/{agent}",
        day=day,
        state_commitment=hashlib.sha256(b"state").digest(),
        cash_microcredits=0,
        component_inventory=tuple(
            InventoryItem(f"inventory/{agent}/{c}", agent, "component", c, 50)
            for c in ("cpu-pintel-2", "board-pintel", "memory-1", "disk-300")
        ),
        pc_inventory=(InventoryItem(f"inventory/{agent}/p01", agent, "pc", "p01", pcs),),
        supplier_offers=(
            SupplierOffer("supplier-offer/1", "alternative-group/1", "supplier-rfq/1", agent, "pintel",
                          "cpu-pintel-2", "on-date", 10, day + 6, 900_000_000, day + 1, 0, None),
        ),
        supplier_orders=(
            SupplierOrder("supplier-order/1", "supplier-offer/0", agent, "pintel", "cpu-pintel-5", 20, day + 2,
                          1_200_000_000, 0, 0, 0),
        ),
        customer_rfqs=(CustomerRfq(f"customer-rfq/{day}/low/0", "p01", 4, day + 8, 2_200_000_000, 100_000_000),),
        active_customer_orders=(
            CustomerOrder("customer-order/1", "customer-rfq/1/low/1", agent, "p01", 3, day + 1, 2_000_000_000,
                          100_000_000, 0, "open", "not-due", None),
        ),
        daily_price_report=DailyPriceReport(day, tuple(PriceRange(f"p{i:02}", None, None) for i in range(1, 17))),
    )


def request(value: DayObservation) -> Message:
    payload = logical_value(value)
    return Message(
        MESSAGE_SCHEMA_ID, MESSAGE_SCHEMA_VERSION, "game/1", "message/engine/1", "engine", "engine",
        value.agent_id, "observation", 1, hashlib.sha256(dumps(payload)).digest(), value, value.day,
    )


class MessageTwoTests(unittest.TestCase):
    def test_schema_is_message_2(self) -> None:
        self.assertEqual((MESSAGE_SCHEMA_ID, MESSAGE_SCHEMA_VERSION), ("tac-scm-message/2", (2, 0, 0)))

    def test_observation_round_trips_with_message_2_fields(self) -> None:
        value = observation()
        raw = encode_message(request(value))
        decoded = decode_message(raw)
        self.assertEqual(decoded.payload, value)
        self.assertEqual(parse_payload(logical_value(value), agent_id="agent-a"), value)

    def test_action_message_uses_message_2_envelope(self) -> None:
        value = observation()
        action = TemplateAgent().act(value)
        message = action_message(request(value), action, sequence=1)
        self.assertEqual((message.schema_id, message.schema_version), (MESSAGE_SCHEMA_ID, MESSAGE_SCHEMA_VERSION))
        self.assertLessEqual(len(encode_message(message)), MAX_ACTION_FRAME_BYTES)


class FrameLimitTests(unittest.TestCase):
    def test_observation_frames_above_two_mib_are_read(self) -> None:
        payload = b"x" * (3 * 1024 * 1024)
        stream = io.BytesIO(len(payload).to_bytes(4, "big") + payload)
        self.assertEqual(read_frame(stream), payload)

    def test_frames_above_the_observation_limit_are_refused(self) -> None:
        stream = io.BytesIO((MAX_OBSERVATION_FRAME_BYTES + 1).to_bytes(4, "big"))
        with self.assertRaises(TransportError):
            read_frame(stream)

    def test_action_frames_keep_the_two_mib_bound(self) -> None:
        with self.assertRaises(TransportError):
            write_frame(io.BytesIO(), b"x" * (MAX_ACTION_FRAME_BYTES + 1))


class SupplierRfqRuleTests(unittest.TestCase):
    @staticmethod
    def rfq(index: int, component: str, quantity: int) -> SupplierRfqCommand:
        return SupplierRfqCommand(f"c-{index}", f"r-{index}", "pintel", component, quantity, 10, None)

    def test_limit_is_per_supplier_component_pair(self) -> None:
        DayActionSet("agent-a", "observation/0/agent-a", 0, 1,
                     supplier_rfqs=(self.rfq(0, "cpu-pintel-2", 0), self.rfq(1, "cpu-pintel-5", 0)))
        DayActionSet("agent-a", "observation/0/agent-a", 0, 1,
                     supplier_rfqs=tuple(self.rfq(i, "cpu-pintel-2" if i < 5 else "cpu-pintel-5", 10) for i in range(10)))
        with self.assertRaises(ContractError):
            DayActionSet("agent-a", "observation/0/agent-a", 0, 1,
                         supplier_rfqs=(self.rfq(0, "cpu-pintel-2", 0), self.rfq(1, "cpu-pintel-2", 0)))
        with self.assertRaises(ContractError):
            DayActionSet("agent-a", "observation/0/agent-a", 0, 1,
                         supplier_rfqs=tuple(self.rfq(i, "cpu-pintel-2", 10) for i in range(6)))


class ActionIdTests(unittest.TestCase):
    def test_command_and_line_ids_share_one_namespace(self) -> None:
        from tac_scm_sdk.contracts import CustomerOfferCommand, ProductionLine

        with self.assertRaises(ContractError):
            DayActionSet("agent-a", "observation/0/agent-a", 0, 1,
                         customer_offers=(CustomerOfferCommand("x1", "customer-rfq/0/low/0", 1),),
                         production_schedule=(ProductionLine("x1", 0, "p01", 1),))

    def test_supplier_rfq_ids_are_unique_per_supplier(self) -> None:
        rfqs = (SupplierRfqCommand("c-0", "r-0", "pintel", "cpu-pintel-2", 5, 10, None),
                SupplierRfqCommand("c-1", "r-0", "pintel", "cpu-pintel-5", 5, 10, None))
        with self.assertRaises(ContractError):
            DayActionSet("agent-a", "observation/0/agent-a", 0, 1, supplier_rfqs=rfqs)
        DayActionSet("agent-a", "observation/0/agent-a", 0, 1, supplier_rfqs=(
            rfqs[0], SupplierRfqCommand("c-1", "r-0", "basus", "board-pintel", 5, 10, None)))


class TemplateTests(unittest.TestCase):
    def test_template_delivers_produces_bids_and_buys(self) -> None:
        action = TemplateAgent().act(observation())
        self.assertEqual([line.customer_order_id for line in action.delivery_schedule], ["customer-order/1"])
        self.assertEqual(action.production_schedule, ())
        self.assertEqual(len(action.customer_offers), 1)
        self.assertEqual(len(action.supplier_orders), 1)
        self.assertTrue(action.supplier_rfqs)
        building = TemplateAgent().act(observation(pcs=0))
        self.assertEqual(building.delivery_schedule, ())
        self.assertEqual([(line.product_id, line.quantity) for line in building.production_schedule], [("p01", 3)])

    def test_template_is_idle_on_the_last_day(self) -> None:
        action = TemplateAgent().act(observation(219))
        self.assertEqual(action.customer_offers + action.supplier_rfqs, ())


if __name__ == "__main__":
    unittest.main()
