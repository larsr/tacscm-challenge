"""Typed, immutable TAC SCM 2026 agent SDK (tac-scm-message/2)."""

from .contracts import (
    ActionCommitted,
    ActionReceipt,
    Agent,
    ContractError,
    DayActionSet,
    DayObservation,
    GameBootstrap,
    GameFinished,
    Message,
    empty_action,
)
from .history import History
from .transport import TransportError, decode_message, encode_message, read_frame, write_frame

__all__ = [
    "ActionCommitted", "ActionReceipt", "Agent", "ContractError", "DayActionSet",
    "DayObservation", "GameBootstrap", "GameFinished", "History", "Message",
    "TransportError", "decode_message", "empty_action", "encode_message",
    "read_frame", "write_frame",
]
