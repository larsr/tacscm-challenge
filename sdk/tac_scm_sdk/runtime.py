"""Fail-closed process runtime for SDK policies."""

from __future__ import annotations

import sys

from .contracts import Agent, DayObservation
from .transport import action_message, decode_message, encode_message, read_frame, write_frame


def serve(agent: Agent) -> int:
    """Serve framed requests until clean EOF; any invalid input exits nonzero."""

    sequence = 0
    try:
        while True:
            raw = read_frame(sys.stdin.buffer)
            if raw is None:
                return 0
            request = decode_message(raw)
            if not isinstance(request.payload, DayObservation):
                # Lifecycle messages are validated and intentionally require no response.
                continue
            action = agent.act(request.payload)
            if (
                action.agent_id != request.payload.agent_id
                or action.day != request.payload.day
                or action.observation_id != request.payload.observation_id
            ):
                raise ValueError("policy returned an action for another observation")
            sequence += 1
            response = action_message(request, action, sequence=sequence)
            write_frame(sys.stdout.buffer, encode_message(response))
    except Exception as error:  # Process isolation converts this to process-exit.
        print(f"tac-scm-agent: {type(error).__name__}: {error}", file=sys.stderr)
        return 2
