"""Agent-local immutable history helpers; no authoritative state is reachable."""

from __future__ import annotations

from dataclasses import dataclass, replace

from .contracts import ActionCommitted, DayActionSet, DayObservation


@dataclass(frozen=True, slots=True)
class History:
    observations: tuple[DayObservation, ...] = ()
    actions: tuple[DayActionSet, ...] = ()
    commits: tuple[ActionCommitted, ...] = ()

    def observe(self, value: DayObservation) -> "History":
        if self.observations and value.day <= self.observations[-1].day:
            raise ValueError("observation history must increase by day")
        return replace(self, observations=self.observations + (value,))

    def acted(self, value: DayActionSet) -> "History":
        if not self.observations or value.observation_id != self.observations[-1].observation_id:
            raise ValueError("action does not bind the latest observation")
        return replace(self, actions=self.actions + (value,))

    def committed(self, value: ActionCommitted) -> "History":
        return replace(self, commits=self.commits + (value,))

    @property
    def latest(self) -> DayObservation | None:
        return self.observations[-1] if self.observations else None
