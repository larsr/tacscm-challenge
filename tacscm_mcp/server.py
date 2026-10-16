"""MCP server: play one seat of a local TAC SCM game from an MCP client.

    uv run --group mcp python tacscm_mcp/server.py

The seat is tacscm_mcp/bridge_agent.py. Each day it waits for submit_action or
autopilot. If neither comes within the game's day timeout, the template
agent's action plays (sdk/template/my_agent.py). Prices in tools are credits;
the protocol's microcredits are converted here.
"""

from __future__ import annotations

from functools import partial
from pathlib import Path
import re
import time
from typing import Any

import anyio.to_thread
from mcp.server.mcpserver import MCPServer
from pydantic import BaseModel, Field

from session import KIT, GameSession

CREDIT = 1_000_000
DEFAULT_OPPONENTS = [
    "tactex-inspired-v4-m5", "deepmaize-inspired-v4-m2", "botticelli-inspired-v4-1",
    "narc-inspired-v1", "iota-inspired-v1",
]
# Keep each tool call under common MCP client timeouts.
MAX_WAIT_S = 50

server = MCPServer(
    "tacscm",
    instructions=(
        "Plays one manufacturer seat in a local TAC SCM game (rules/RULES.md). Call start_game, then repeat: "
        "observe, then submit_action or autopilot. Everything submitted on day d takes effect on day d+1. "
        "Prices are in credits. The score is the final cash. If you do not answer within day_timeout_s, the "
        "template agent's suggestion plays for that day."
    ),
)
session: GameSession | None = None


class CustomerOffer(BaseModel):
    rfq_id: str
    unit_price: float = Field(description="credits per unit; wins if lowest and at most the RFQ's reserve")


class SupplierRfq(BaseModel):
    supplier_id: str
    component_id: str
    quantity: int = Field(ge=0, description="0 is a price probe")
    due_day: int = Field(ge=0, le=219)
    max_unit_price: float | None = Field(default=None, description="credits; null for no limit")


class Production(BaseModel):
    product_id: str
    quantity: int = Field(ge=1)


def credits(micro: int | None) -> float | None:
    return None if micro is None else round(micro / CREDIT, 2)


def micro(value: float | None) -> int | None:
    return None if value is None else round(value * CREDIT)


def counts(items: list[dict]) -> dict[str, int]:
    result: dict[str, int] = {}
    for item in items:
        if item["quantity"]:
            result[item["item_id"]] = result.get(item["item_id"], 0) + item["quantity"]
    return result


def simplify_action(action: dict) -> dict:
    return {
        "customer_offers": [{"rfq_id": o["rfq_id"], "unit_price": credits(o["unit_price"])} for o in action["customer_offers"]],
        "supplier_rfqs": [
            {"supplier_id": r["supplier_id"], "component_id": r["component_id"], "quantity": r["quantity"],
             "due_day": r["due_day"], "max_unit_price": credits(r["max_unit_price"])}
            for r in action["supplier_rfqs"]
        ],
        "supplier_orders": [o["offer_id"] for o in action["supplier_orders"]],
        "production": [{"product_id": p["product_id"], "quantity": p["quantity"]} for p in action["production_schedule"]],
        "deliveries": [d["customer_order_id"] for d in action["delivery_schedule"]],
    }


def summarize(game: GameSession, observation: dict, max_rfqs: int) -> dict[str, Any]:
    day = observation["day"]
    rfqs = sorted(observation["customer_rfqs"], key=lambda r: -r["unit_reserve"])
    committed = game.committed.get(day - 1)
    yesterday: dict[str, Any] = {"source": game.sources.get(day - 1)}
    if committed is not None:
        yesterday["cause"] = committed["cause"]
        yesterday["rejected_commands"] = [r for r in committed["command_results"] if not r["accepted"]]
    yesterday["failed_lines"] = [
        r for r in observation["production_results"] + observation["delivery_results"] if not r["executed"]
    ]
    if day - 1 in game.receipt_errors:
        yesterday["receipt_errors"] = game.receipt_errors[day - 1]
    pipeline: dict[str, int] = {}
    for order in observation["supplier_orders"]:
        remaining = order["quantity"] - order["delivered_quantity"]
        if remaining > 0:
            pipeline[order["component_id"]] = pipeline.get(order["component_id"], 0) + remaining
    return {
        "day": day,
        "days_left": 219 - day,
        "answer_within_s": game.day_timeout_s,
        "cash": credits(observation["cash_microcredits"]),
        "inventory": counts(observation["component_inventory"] + observation["pc_inventory"]),
        "arriving_tomorrow": counts(observation["pending_inventory"]),
        "on_order_from_suppliers": pipeline,
        "customer_orders": [
            {"order_id": o["order_id"], "product_id": o["product_id"], "quantity": o["quantity"], "due_day": o["due_day"],
             "unit_price": credits(o["unit_price"]), "daily_penalty": credits(o["daily_penalty"]), "status": o["status"]}
            for o in observation["active_customer_orders"]
        ],
        "customer_rfqs_total": len(rfqs),
        "customer_rfqs": [
            {"rfq_id": r["rfq_id"], "product_id": r["product_id"], "quantity": r["quantity"], "due_day": r["due_day"],
             "reserve": credits(r["unit_reserve"]), "daily_penalty": credits(r["daily_penalty"])}
            for r in rfqs[:max_rfqs]
        ],
        "supplier_offers": [
            {"offer_id": o["offer_id"], "group": o["alternative_group_id"], "supplier_id": o["supplier_id"],
             "component_id": o["component_id"], "kind": o["kind"], "quantity": o["quantity"], "due_day": o["due_day"],
             "unit_price": credits(o["unit_price"]), "no_offer_reason": o["no_offer_reason"]}
            for o in observation["supplier_offers"]
        ],
        "supplier_reputation_ppm": {
            f"{r['supplier_id']}/{r['component_id']}": r["reputation_ppm"] for r in observation["supplier_reputations"]
        },
        "yesterday": yesterday,
        "price_report": {
            p["product_id"]: [credits(p["minimum"]), credits(p["maximum"])]
            for p in (observation["daily_price_report"] or {}).get("products", [])
        },
        "market_report": observation["market_report"],
        "template_suggestion": simplify_action(game.suggestion) if game.suggestion else None,
    }


def require_session() -> GameSession:
    if session is None:
        raise ValueError("no game; call start_game first")
    return session


@server.tool()
async def start_game(
    opponents: list[str] | None = None, root: int = 0, seat: int = 5, day_timeout_s: int = 300, name: str | None = None,
) -> dict:
    """Start a local game: five house agents (agents/README.md) and this seat. `root` picks the game's random
    seed; `seat` is 0..5; `day_timeout_s` is how long each day waits for you before the template plays it."""

    global session
    if session is not None and session.running:
        raise ValueError("a game is running; call stop_game first")
    name = name or time.strftime("mcp-%Y%m%d-%H%M%S")
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", name) or name.startswith("."):
        raise ValueError("name must be letters, digits, '.', '_' or '-'")
    session = GameSession(
        opponents or DEFAULT_OPPONENTS, root=root, out=KIT / "runs" / name, seat=seat, day_timeout_s=day_timeout_s,
    )
    session.start()
    return {"out": str(session.out.relative_to(KIT)), "seats": session.opponents[:seat] + ["(you)"] + session.opponents[seat:],
            "note": "the runner waits 30 s before day 0; call observe"}


@server.tool()
async def catalog() -> dict:
    """The game's catalog (products, components with base prices in microcredits, suppliers) and its
    sampled configuration (interest and storage rates, initial demand)."""

    game = require_session()
    if game.bootstrap is None:
        await anyio.to_thread.run_sync(partial(game.wait_for_day, MAX_WAIT_S))
    if game.bootstrap is None:
        raise ValueError("no bootstrap yet; try again")
    return {"agent_id": game.bootstrap["agent_id"], "catalog": game.bootstrap["catalog"],
            "configuration": game.bootstrap["semantic_configuration"]}


@server.tool()
async def observe(max_rfqs: int = 30, wait_s: int = MAX_WAIT_S) -> dict:
    """Wait for the day that needs your action and summarize it: cash, inventory, orders, today's customer
    RFQs (highest reserve first), supplier offers, yesterday's rejections, prices and the template's suggestion."""

    game = require_session()
    observation = await anyio.to_thread.run_sync(partial(game.wait_for_day, min(wait_s, MAX_WAIT_S)))
    if observation is None:
        return await game_status()
    return summarize(game, observation, max_rfqs)


@server.tool()
async def submit_action(
    customer_offers: list[CustomerOffer] | None = None,
    supplier_rfqs: list[SupplierRfq] | None = None,
    supplier_orders: list[str] | None = None,
    production: list[Production] | None = None,
    deliveries: list[str] | None = None,
) -> dict:
    """Submit the complete action for today (not a patch; anything omitted is not done). supplier_orders are
    offer ids; deliveries are customer order ids (delivered in full); production and deliveries run in the
    order given. Economic rejections are reported in the next observe under `yesterday`."""

    game = require_session()
    observation = game.observation
    if observation is None or game.pending_day is None:
        raise ValueError("no day is waiting for an action; call observe")
    day = observation["day"]
    orders = {o["order_id"]: o for o in observation["active_customer_orders"]}
    unknown = [order_id for order_id in deliveries or [] if order_id not in orders]
    if unknown:
        raise ValueError(f"unknown customer orders: {unknown}")
    action = {
        "customer_offers": [
            {"command_id": f"o{i}", "rfq_id": o.rfq_id, "unit_price": micro(o.unit_price)}
            for i, o in enumerate(customer_offers or [])
        ],
        "supplier_rfqs": [
            {"command_id": f"s{i}", "rfq_id": f"r{day}-{i}", "supplier_id": r.supplier_id, "component_id": r.component_id,
             "quantity": r.quantity, "due_day": r.due_day, "max_unit_price": micro(r.max_unit_price)}
            for i, r in enumerate(supplier_rfqs or [])
        ],
        "supplier_orders": [{"command_id": f"a{i}", "offer_id": offer_id} for i, offer_id in enumerate(supplier_orders or [])],
        "production_schedule": [
            {"line_id": f"p{i}", "priority": i, "product_id": p.product_id, "quantity": p.quantity}
            for i, p in enumerate(production or [])
        ],
        "delivery_schedule": [
            {"line_id": f"d{i}", "priority": i, "customer_order_id": order_id, "quantity": orders[order_id]["quantity"]}
            for i, order_id in enumerate(deliveries or [])
        ],
    }
    error = await anyio.to_thread.run_sync(partial(game.submit, action))
    if error is not None:
        return {"day": day, "sent": False, "error": error}
    return {"day": day, "sent": True}


@server.tool()
async def autopilot(days: int = 1) -> dict:
    """Let the template agent play the next `days` days (1 = accept today's suggestion). Returns when done or
    after about 50 s; the rest then continues in the background."""

    game = require_session()
    last = await anyio.to_thread.run_sync(partial(game.autopilot, days, MAX_WAIT_S))
    return {"played_through_day": last, "autopilot_until": game.autopilot_until,
            "cash": credits(game.cash.get(max(game.cash), 0)) if game.cash else None, "running": game.running}


@server.tool()
async def game_status() -> dict:
    """Whether the game runs, the current day, cash by day (every 10th), who decided each day, and the final
    result once the game is over."""

    game = require_session()
    sources = list(game.sources.values())
    status: dict[str, Any] = {
        "running": game.running,
        "waiting_for_day": game.pending_day,
        "last_day_played": game.last_day,
        "cash_by_day": {day: credits(cash) for day, cash in game.cash.items() if day % 10 == 0 or day == max(game.cash)},
        "days_by_you": sources.count("controller"),
        "days_by_template": sources.count("template"),
        "autopilot_until": game.autopilot_until,
    }
    if not game.running:
        result = game.result()
        status["result"] = None if result is None else [
            {"seat": s["seat"], "agent": Path(s["agent"]).name, "score": credits(s["score_microcredits"]),
             "default_days": s["default_days"]}
            for s in result["seats"]
        ]
        if result is None:
            status["runner_log"] = game.log_tail()
    return status


@server.tool()
async def stop_game() -> dict:
    """Stop the running game."""

    game = require_session()
    await anyio.to_thread.run_sync(game.stop)
    return {"stopped": True}


if __name__ == "__main__":
    try:
        server.run()
    finally:
        if session is not None:
            session.stop()
