"""Closed public contracts for gallery decisions and learning trajectories."""

from __future__ import annotations

from dataclasses import dataclass
import re
from typing import Protocol, runtime_checkable

from tac_scm_sdk.contracts import DayActionSet, DayObservation, GameBootstrap, GameFinished


EXTERNAL_ID = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
PARTITIONS = frozenset({"train", "development", "validation", "test"})
FEATURE_SCHEMA = "tac-scm-gallery-features/v1"
EXPLANATION_SCHEMA = "tac-scm-gallery-explanation/v1"
TRAJECTORY_SCHEMA = "tac-scm-gallery-trajectory/v1"


def _identifier(value: str, name: str) -> None:
    if EXTERNAL_ID.fullmatch(value) is None:
        raise ValueError(f"{name} is not a bounded lowercase identifier")


@dataclass(frozen=True, slots=True)
class EpisodeDescriptor:
    """Identity supplied by the experiment, never by hidden engine state."""

    experiment_id: str
    partition: str
    artifact_id: str
    artifact_sha256: str
    instance_id: str
    block_index: int

    def __post_init__(self) -> None:
        for value, name in (
            (self.experiment_id, "experiment id"),
            (self.artifact_id, "artifact id"),
            (self.instance_id, "instance id"),
        ):
            _identifier(value, name)
        if self.partition not in PARTITIONS:
            raise ValueError("unknown episode partition")
        if len(self.artifact_sha256) != 64 or any(
            char not in "0123456789abcdef" for char in self.artifact_sha256
        ):
            raise ValueError("artifact SHA-256 must be lowercase hexadecimal")
        if not 0 <= self.block_index <= 0xFFFF_FFFF:
            raise ValueError("block index is out of range")


@dataclass(frozen=True, slots=True)
class FeatureVector:
    """A fixed-order integer vector derived only from one public observation."""

    day: int
    names: tuple[str, ...]
    values: tuple[int, ...]
    schema: str = FEATURE_SCHEMA

    def __post_init__(self) -> None:
        if self.schema != FEATURE_SCHEMA:
            raise ValueError("unsupported feature schema")
        if not 0 <= self.day <= 219:
            raise ValueError("feature day is out of range")
        if not self.names or len(self.names) != len(self.values):
            raise ValueError("feature names and values differ")
        if len(self.names) != len(set(self.names)):
            raise ValueError("feature names are duplicated")
        for name in self.names:
            _identifier(name, "feature name")
        for value in self.values:
            if isinstance(value, bool) or not isinstance(value, int):
                raise ValueError("features must use exact integers")

    def mapping(self) -> dict[str, int]:
        return dict(zip(self.names, self.values, strict=True))


@dataclass(frozen=True, slots=True)
class DecisionExplanation:
    """Bounded strategy trace; public visibility is decided outside the agent."""

    strategy_id: str
    mode: str
    summary: str
    reasons: tuple[str, ...] = ()
    metrics: tuple[tuple[str, int], ...] = ()
    confidence_ppm: int | None = None
    schema: str = EXPLANATION_SCHEMA

    def __post_init__(self) -> None:
        if self.schema != EXPLANATION_SCHEMA:
            raise ValueError("unsupported explanation schema")
        _identifier(self.strategy_id, "strategy id")
        _identifier(self.mode, "strategy mode")
        if not 1 <= len(self.summary.encode("utf-8")) <= 512:
            raise ValueError("explanation summary must contain 1..512 bytes")
        if len(self.reasons) > 16 or any(
            not 1 <= len(reason.encode("utf-8")) <= 256 for reason in self.reasons
        ):
            raise ValueError("explanation reasons exceed their bound")
        names = [name for name, _ in self.metrics]
        if len(names) > 32 or len(names) != len(set(names)):
            raise ValueError("explanation metrics are duplicated or excessive")
        for name, value in self.metrics:
            _identifier(name, "metric name")
            if isinstance(value, bool) or not isinstance(value, int):
                raise ValueError("explanation metrics must use exact integers")
        if self.confidence_ppm is not None and not 0 <= self.confidence_ppm <= 1_000_000:
            raise ValueError("confidence must be in 0..1,000,000 ppm")


@dataclass(frozen=True, slots=True)
class KernelDecision:
    action: DayActionSet
    explanation: DecisionExplanation


@runtime_checkable
class GalleryKernel(Protocol):
    """One strategy kernel over public history and the current observation."""

    strategy_id: str

    def decide(
        self,
        observation: DayObservation,
        features: FeatureVector,
        history: "PublicHistoryProtocol",
    ) -> KernelDecision:
        """Return a complete proposed action and a bounded explanation."""


class PublicHistoryProtocol(Protocol):
    @property
    def observations(self) -> tuple[DayObservation, ...]: ...

    @property
    def features(self) -> tuple[FeatureVector, ...]: ...


class TrajectorySink(Protocol):
    def begin(self, bootstrap: GameBootstrap) -> None: ...

    def decision(
        self,
        observation: DayObservation,
        features: FeatureVector,
        action: DayActionSet,
        explanation: DecisionExplanation,
        safety_reasons: tuple[str, ...],
        reward_since_previous: int,
    ) -> None: ...

    def finish(self, result: GameFinished) -> None: ...


class NullTrajectorySink:
    """Normal operation retains no trajectory bytes."""

    def begin(self, bootstrap: GameBootstrap) -> None:
        del bootstrap

    def decision(
        self,
        observation: DayObservation,
        features: FeatureVector,
        action: DayActionSet,
        explanation: DecisionExplanation,
        safety_reasons: tuple[str, ...],
        reward_since_previous: int,
    ) -> None:
        del observation, features, action, explanation, safety_reasons
        del reward_since_previous

    def finish(self, result: GameFinished) -> None:
        del result
