"""Dependency-free deterministic CBOR for the TAC SCM process boundary.

The supported value domain is deliberately smaller than general CBOR: signed
integers, byte/text strings, arrays, text-keyed maps, booleans, and null.
Floats, tags, indefinite lengths, duplicate keys, and non-normalized text are
rejected before policy code sees a message.
"""

from __future__ import annotations

from dataclasses import dataclass
import unicodedata
from typing import Any


class CborError(ValueError):
    """A value or byte string is outside the canonical transport domain."""


def _head(major: int, value: int) -> bytes:
    if value < 0:
        raise CborError("CBOR length cannot be negative")
    prefix = major << 5
    if value < 24:
        return bytes((prefix | value,))
    if value <= 0xFF:
        return bytes((prefix | 24, value))
    if value <= 0xFFFF:
        return bytes((prefix | 25,)) + value.to_bytes(2, "big")
    if value <= 0xFFFF_FFFF:
        return bytes((prefix | 26,)) + value.to_bytes(4, "big")
    if value <= 0xFFFF_FFFF_FFFF_FFFF:
        return bytes((prefix | 27,)) + value.to_bytes(8, "big")
    raise CborError("integer exceeds the 64-bit CBOR domain")


def dumps(value: Any) -> bytes:
    """Encode one value using RFC 8949 deterministic encoding."""

    if value is None:
        return b"\xf6"
    if value is False:
        return b"\xf4"
    if value is True:
        return b"\xf5"
    if isinstance(value, int):
        if value >= 0:
            return _head(0, value)
        return _head(1, -1 - value)
    if isinstance(value, bytes):
        return _head(2, len(value)) + value
    if isinstance(value, str):
        if unicodedata.normalize("NFC", value) != value:
            raise CborError("text must be NFC-normalized")
        raw = value.encode("utf-8", errors="strict")
        return _head(3, len(raw)) + raw
    if isinstance(value, (list, tuple)):
        return _head(4, len(value)) + b"".join(dumps(item) for item in value)
    if isinstance(value, dict):
        encoded: list[tuple[bytes, bytes]] = []
        for key, item in value.items():
            if not isinstance(key, str):
                raise CborError("map keys must be text")
            encoded.append((dumps(key), dumps(item)))
        encoded.sort(key=lambda pair: (len(pair[0]), pair[0]))
        return _head(5, len(encoded)) + b"".join(k + v for k, v in encoded)
    raise CborError(f"unsupported CBOR type: {type(value).__name__}")


@dataclass(slots=True)
class _Decoder:
    raw: bytes
    offset: int = 0

    def take(self, length: int) -> bytes:
        end = self.offset + length
        if end > len(self.raw):
            raise CborError("truncated CBOR value")
        value = self.raw[self.offset:end]
        self.offset = end
        return value

    def argument(self, additional: int) -> int:
        if additional < 24:
            return additional
        sizes = {24: 1, 25: 2, 26: 4, 27: 8}
        if additional not in sizes:
            raise CborError("indefinite or reserved CBOR argument")
        raw = self.take(sizes[additional])
        value = int.from_bytes(raw, "big")
        minimum = {24: 24, 25: 256, 26: 65536, 27: 4294967296}[additional]
        if value < minimum:
            raise CborError("non-preferred CBOR integer or length")
        return value

    def value(self, depth: int = 0) -> Any:
        if depth > 64:
            raise CborError("CBOR nesting exceeds 64 levels")
        initial = self.take(1)[0]
        major, additional = initial >> 5, initial & 31
        if major == 7:
            if additional == 20:
                return False
            if additional == 21:
                return True
            if additional == 22:
                return None
            raise CborError("floats and unsupported CBOR simple values are forbidden")
        argument = self.argument(additional)
        if major == 0:
            return argument
        if major == 1:
            return -1 - argument
        if major == 2:
            return self.take(argument)
        if major == 3:
            try:
                text = self.take(argument).decode("utf-8", errors="strict")
            except UnicodeDecodeError as error:
                raise CborError("invalid UTF-8 text") from error
            if unicodedata.normalize("NFC", text) != text:
                raise CborError("text must be NFC-normalized")
            return text
        if major == 4:
            return [self.value(depth + 1) for _ in range(argument)]
        if major == 5:
            result: dict[str, Any] = {}
            previous: tuple[int, bytes] | None = None
            for _ in range(argument):
                key_start = self.offset
                key = self.value(depth + 1)
                key_raw = self.raw[key_start:self.offset]
                if not isinstance(key, str):
                    raise CborError("map keys must be text")
                order = (len(key_raw), key_raw)
                if previous is not None and order <= previous:
                    raise CborError("map keys are duplicated or not canonically ordered")
                previous = order
                result[key] = self.value(depth + 1)
            return result
        raise CborError("CBOR tags are forbidden")


def loads(raw: bytes) -> Any:
    """Decode exactly one canonical value and reject every trailing byte."""

    decoder = _Decoder(raw)
    value = decoder.value()
    if decoder.offset != len(raw):
        raise CborError("trailing bytes after CBOR value")
    if dumps(value) != raw:
        raise CborError("CBOR value is not canonical")
    return value
