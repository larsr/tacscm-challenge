"""Lifecycle-aware local process runtime for gallery policies."""

from __future__ import annotations

import sys

from tac_scm_sdk.contracts import DayObservation, GameBootstrap, GameFinished
from tac_scm_sdk.transport import (
    action_message,
    decode_message,
    encode_message,
    read_frame,
    write_frame,
)

from .agent import GalleryAgent


def serve_gallery(agent: GalleryAgent) -> int:
    """Serve one framed episode and notify the gallery lifecycle hooks."""

    sequence = 0
    try:
        while True:
            raw = read_frame(sys.stdin.buffer)
            if raw is None:
                return 0
            request = decode_message(raw)
            if isinstance(request.payload, GameBootstrap):
                agent.begin(request.payload)
                continue
            if isinstance(request.payload, GameFinished):
                agent.finish(request.payload)
                continue
            if not isinstance(request.payload, DayObservation):
                continue
            action = agent.act(request.payload)
            sequence += 1
            response = action_message(request, action, sequence=sequence)
            write_frame(sys.stdout.buffer, encode_message(response))
    except Exception as error:
        print(f"tac-scm-gallery-agent: {type(error).__name__}: {error}", file=sys.stderr)
        return 2
