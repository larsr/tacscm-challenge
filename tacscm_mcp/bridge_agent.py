#!/usr/bin/env python3
"""A seat whose daily decisions come from an outside controller (the MCP server).

The game runner launches this like any agent. It connects to the Unix socket
named by TACSCM_BRIDGE_SOCKET, sends every message it receives as one JSON
line, and for each day waits up to TACSCM_BRIDGE_TIMEOUT_S seconds for an
action. If no answer arrives in time, or the controller is missing or broken,
the template agent's action plays, so the seat never defaults.

Controller -> bridge, one JSON line each:
  {"type": "action", "day": D, "action": {the five day-action-set arrays}}
  {"type": "autopilot", "day": D}
"""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import socket
import sys
import time
from typing import Any

from tac_scm_sdk.contracts import ContractError, DayActionSet, DayObservation, logical_value, parse_payload
from tac_scm_sdk.transport import action_message, decode_message, encode_message, read_frame, write_frame

KIT = Path(__file__).resolve().parent.parent


def load_template() -> Any:
    spec = importlib.util.spec_from_file_location("tacscm_template_agent", KIT / "sdk" / "template" / "my_agent.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.TemplateAgent()


def plain(value: Any) -> Any:
    """A logical value as JSON: bytes become hex."""

    if isinstance(value, bytes):
        return value.hex()
    if isinstance(value, dict):
        return {key: plain(item) for key, item in value.items()}
    if isinstance(value, list):
        return [plain(item) for item in value]
    return value


def log(text: str) -> None:
    print(f"tacscm-bridge: {text}", file=sys.stderr, flush=True)


class Controller:
    def __init__(self, path: str | None) -> None:
        self.sock: socket.socket | None = None
        self.buffer = b""
        if not path:
            log("no TACSCM_BRIDGE_SOCKET; playing the template")
            return
        try:
            self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self.sock.connect(path)
        except OSError as error:
            log(f"cannot connect to {path}: {error}; playing the template")
            self.sock = None

    def close(self) -> None:
        if self.sock is not None:
            self.sock.close()
            self.sock = None

    def send(self, message: dict) -> None:
        if self.sock is None:
            return
        try:
            self.sock.settimeout(5)
            self.sock.sendall(json.dumps(message, separators=(",", ":")).encode() + b"\n")
        except OSError as error:
            log(f"controller lost: {error}")
            self.close()

    def receive(self, deadline: float) -> dict | None:
        while self.sock is not None and b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            self.sock.settimeout(remaining)
            try:
                chunk = self.sock.recv(65536)
            except TimeoutError:
                return None
            except OSError as error:
                log(f"controller lost: {error}")
                self.close()
                return None
            if not chunk:
                self.close()
                return None
            self.buffer += chunk
        if self.sock is None:
            return None
        line, _, self.buffer = self.buffer.partition(b"\n")
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            return {}


def decide(controller: Controller, observation: DayObservation, suggestion: DayActionSet, deadline: float) -> DayActionSet:
    while controller.sock is not None:
        reply = controller.receive(deadline)
        if reply is None:
            break
        if reply.get("day") != observation.day:
            continue  # a late answer for an earlier day
        if reply.get("type") == "autopilot":
            break
        try:
            action = parse_payload({
                "type": "day-action-set", "agent_id": observation.agent_id,
                "observation_id": observation.observation_id, "day": observation.day, "revision": 1,
                **reply["action"],
            })
        except (ContractError, KeyError, TypeError, ValueError) as error:
            controller.send({"type": "action-invalid", "day": observation.day, "error": str(error)})
            continue
        controller.send({"type": "action-sent", "day": observation.day, "source": "controller"})
        assert isinstance(action, DayActionSet)
        return action
    controller.send({"type": "action-sent", "day": observation.day, "source": "template"})
    return suggestion


def main() -> int:
    template = load_template()
    controller = Controller(os.environ.get("TACSCM_BRIDGE_SOCKET"))
    timeout = float(os.environ.get("TACSCM_BRIDGE_TIMEOUT_S", "10"))
    sequence = 0
    try:
        while True:
            raw = read_frame(sys.stdin.buffer)
            if raw is None:
                return 0
            request = decode_message(raw)
            payload = request.payload
            if not isinstance(payload, DayObservation):
                controller.send({"type": payload.type, "payload": plain(logical_value(payload))})
                continue
            suggestion = template.act(payload)
            controller.send({
                "type": "day-observation", "day": payload.day, "timeout_s": timeout,
                "observation": plain(logical_value(payload)), "suggestion": plain(logical_value(suggestion)),
            })
            action = decide(controller, payload, suggestion, time.monotonic() + timeout)
            sequence += 1
            write_frame(sys.stdout.buffer, encode_message(action_message(request, action, sequence=sequence)))
    except Exception as error:
        log(f"{type(error).__name__}: {error}")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
