"""Shared stateful agent wrapper around one strategy kernel."""

from __future__ import annotations

from tac_scm_sdk.contracts import DayActionSet, DayObservation, GameBootstrap, GameFinished

from .contracts import GalleryKernel, NullTrajectorySink, TrajectorySink
from .history import PublicHistory
from .safety import project_safe_action


class GalleryAgent:
    """Accumulate public history, project safety, explain, and optionally record."""

    def __init__(
        self, kernel: GalleryKernel, sink: TrajectorySink | None = None
    ) -> None:
        self.kernel = kernel
        self.sink = sink if sink is not None else NullTrajectorySink()
        self.history = PublicHistory()
        self._last_cash: int | None = None

    def reset(self, sink: TrajectorySink | None = None) -> None:
        """Clear episode-local state and optionally install a fresh trajectory sink."""

        self.history = PublicHistory()
        self._last_cash = None
        if sink is not None:
            self.sink = sink

    def begin(self, bootstrap: GameBootstrap) -> None:
        if self.history.observations:
            raise ValueError("cannot begin an episode with populated history")
        self.sink.begin(bootstrap)

    def act(self, observation: DayObservation) -> DayActionSet:
        history, features = self.history.observe(observation)
        decision = self.kernel.decide(observation, features, history)
        if decision.explanation.strategy_id != self.kernel.strategy_id:
            raise ValueError("explanation strategy does not match the kernel")
        projection = project_safe_action(observation, decision.action)
        reward = (
            0
            if self._last_cash is None
            else observation.cash_microcredits - self._last_cash
        )
        self.sink.decision(
            observation,
            features,
            projection.action,
            decision.explanation,
            projection.reasons,
            reward,
        )
        self.history = history
        self._last_cash = observation.cash_microcredits
        return projection.action

    def finish(self, result: GameFinished) -> None:
        self.sink.finish(result)
