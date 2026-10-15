"""Canonical CBOR envelope and length-prefixed stdio transport."""

from __future__ import annotations

import hashlib
from typing import IO

from . import cbor
from .contracts import (
    MESSAGE_SCHEMA_ID,
    MESSAGE_SCHEMA_VERSION,
    ContractError,
    DayActionSet,
    Message,
    logical_value,
    parse_payload,
)


# Frames the agent writes carry one day's action set, which the rules bound at
# 2 MiB of canonical CBOR; the runner enforces the same bound.
MAX_ACTION_FRAME_BYTES = 2 * 1024 * 1024
# Frames the agent reads carry the runner's messages, chiefly the daily
# observation. No rule bounds them, and an observation grows with an agent's
# open positions (under the 2026-draft2 profile it also lists every supplier
# order ever placed and passed 2 MiB near day 180 for agents that order
# daily). 32 MiB is far above any observation seen in a 220-day game under
# either profile.
MAX_OBSERVATION_FRAME_BYTES = 32 * 1024 * 1024
# Kept for compatibility: the action bound.
MAX_FRAME_BYTES = MAX_ACTION_FRAME_BYTES


class TransportError(RuntimeError):
    """A framed stream or envelope failed before policy execution."""


def read_exact(stream: IO[bytes], length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        part = stream.read(length - len(chunks))
        if not part:
            raise TransportError(f"unexpected EOF with {length - len(chunks)} bytes remaining")
        chunks.extend(part)
    return bytes(chunks)


def read_frame(stream: IO[bytes], *, limit: int = MAX_OBSERVATION_FRAME_BYTES) -> bytes | None:
    first = stream.read(1)
    if first == b"":
        return None
    prefix = first + read_exact(stream, 3)
    length = int.from_bytes(prefix, "big")
    if length > limit:
        raise TransportError(f"frame length {length} exceeds declared limit {limit}")
    return read_exact(stream, length)


def write_frame(stream: IO[bytes], payload: bytes, *, limit: int = MAX_ACTION_FRAME_BYTES) -> None:
    if len(payload) > limit:
        raise TransportError(f"frame length {len(payload)} exceeds declared limit {limit}")
    stream.write(len(payload).to_bytes(4, "big"))
    stream.write(payload)
    stream.flush()


def decode_message(raw: bytes) -> Message:
    try:
        value = cbor.loads(raw)
    except cbor.CborError as error:
        raise TransportError(f"non-canonical CBOR: {error}") from error
    if not isinstance(value, dict):
        raise TransportError("message must be a map")
    required = {
        "schema_id", "schema_version", "game_id", "message_id", "sender_role",
        "sender_id", "recipient", "phase", "sender_sequence", "payload_digest", "payload",
    }
    optional = {"game_day", "causation_id", "final_submission"}
    missing, unknown = required - set(value), set(value) - required - optional
    if missing or unknown:
        raise TransportError(f"closed envelope fields missing={sorted(missing)} unknown={sorted(unknown)}")
    payload_bytes = cbor.dumps(value["payload"])
    actual = hashlib.sha256(payload_bytes).digest()
    if value["payload_digest"] != actual:
        raise TransportError("payload digest mismatch")
    try:
        payload = parse_payload(value["payload"], agent_id=value["recipient"])
        version = value["schema_version"]
        if not isinstance(version, list):
            raise ContractError("schema_version must be an array")
        return Message(
            value["schema_id"], tuple(version), value["game_id"], value["message_id"],
            value["sender_role"], value["sender_id"], value["recipient"], value["phase"],
            value["sender_sequence"], value["payload_digest"], payload,
            value.get("game_day"), value.get("causation_id"), value.get("final_submission"),
        )
    except (ContractError, TypeError) as error:
        raise TransportError(f"schema-invalid: {error}") from error


def encode_message(message: Message) -> bytes:
    value = logical_value(message)
    payload = value["payload"]
    digest = hashlib.sha256(cbor.dumps(payload)).digest()
    if digest != message.payload_digest:
        raise TransportError("message payload_digest does not match payload")
    return cbor.dumps(value)


def action_message(request: Message, action: DayActionSet, *, sequence: int) -> Message:
    """Bind a policy action to the game and observation envelope."""

    payload = logical_value(action)
    digest = hashlib.sha256(cbor.dumps(payload)).digest()
    return Message(
        MESSAGE_SCHEMA_ID, MESSAGE_SCHEMA_VERSION, request.game_id,
        f"message/{action.agent_id}/{action.day}/{sequence}", "manufacturer",
        action.agent_id, "runner", "action-window", sequence, digest, action,
        game_day=action.day, final_submission=True,
    )
