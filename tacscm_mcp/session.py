"""One local game in which one seat is played from outside, through bridge_agent.py.

Runs runner/play_game.py with the bridge in one seat and serves the bridge's
Unix socket. The MCP server drives it; a reinforcement-learning environment
could drive it the same way (wait_for_day, then submit or autopilot).
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

KIT = Path(__file__).resolve().parent.parent
BRIDGE = Path(__file__).resolve().parent / "bridge_agent.py"
# Runner deadline beyond the bridge's own timeout, so the template fallback always lands in time.
DEADLINE_MARGIN_S = 30


class GameSession:
    def __init__(self, opponents: list[str], *, root: int, out: Path, seat: int, day_timeout_s: float) -> None:
        if len(opponents) != 5:
            raise ValueError("five opponents are needed")
        if not 0 <= seat <= 5:
            raise ValueError("seat must be 0..5")
        if out.exists():
            raise ValueError(f"{out} exists")
        self.opponents, self.root, self.out, self.seat = list(opponents), root, out, seat
        self.day_timeout_s = day_timeout_s
        self.cond = threading.Condition()
        self.send_lock = threading.Lock()
        self.connection: socket.socket | None = None
        self.bootstrap: dict | None = None
        self.observation: dict | None = None  # the day awaiting an answer
        self.suggestion: dict | None = None
        self.pending_day: int | None = None
        self.last_day = -1
        self.invalid: dict | None = None
        self.committed: dict[int, dict] = {}
        self.receipt_errors: dict[int, list] = {}
        self.sources: dict[int, str] = {}
        self.cash: dict[int, int] = {}
        self.finished: dict | None = None
        self.autopilot_until = -1
        self.closed = False

    # Lifecycle

    def start(self) -> None:
        self.socket_dir = tempfile.mkdtemp(prefix="tacscm-")
        path = os.path.join(self.socket_dir, "bridge.sock")
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(path)
        self.listener.listen(1)
        seats = list(self.opponents)
        seats.insert(self.seat, str(BRIDGE))
        command = [
            sys.executable, str(KIT / "runner" / "play_game.py"), "--root", str(self.root), "--out", str(self.out),
            "--deadline-ms", str(int((self.day_timeout_s + DEADLINE_MARGIN_S) * 1000)), *seats,
        ]
        environment = {**os.environ, "TACSCM_BRIDGE_SOCKET": path, "TACSCM_BRIDGE_TIMEOUT_S": str(self.day_timeout_s)}
        self.log_path = self.out.with_name(self.out.name + ".log")
        self.out.parent.mkdir(parents=True, exist_ok=True)
        with open(self.log_path, "wb") as log:
            self.process = subprocess.Popen(
                command, cwd=KIT, env=environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        threading.Thread(target=self._serve, daemon=True).start()

    def stop(self) -> None:
        if self.process.poll() is None:
            # The whole group: the runner, the game server and all six seats.
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(10)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
        with self.cond:
            self.closed = True
            self.cond.notify_all()

    @property
    def running(self) -> bool:
        return self.process.poll() is None

    def result(self) -> dict | None:
        path = self.out / "result.json"
        return json.loads(path.read_text()) if path.is_file() else None

    def log_tail(self, lines: int = 20) -> str:
        return "\n".join(self.log_path.read_text(errors="replace").splitlines()[-lines:])

    # Bridge connection

    def _serve(self) -> None:
        self.listener.settimeout(1.0)
        try:
            while self.connection is None:
                if self.process.poll() is not None:
                    return
                try:
                    self.connection, _ = self.listener.accept()
                except TimeoutError:
                    continue
            with self.connection.makefile("r", encoding="utf-8") as reader:
                for line in reader:
                    self._handle(json.loads(line))
        finally:
            self.listener.close()
            shutil.rmtree(self.socket_dir, ignore_errors=True)
            with self.cond:
                self.closed = True
                self.pending_day = None
                self.cond.notify_all()

    def _send(self, message: dict) -> None:
        if self.connection is None:
            raise RuntimeError("the bridge is not connected")
        with self.send_lock:
            self.connection.sendall(json.dumps(message, separators=(",", ":")).encode() + b"\n")

    def _handle(self, message: dict) -> None:
        kind = message.get("type")
        with self.cond:
            if kind == "game-bootstrap":
                self.bootstrap = message["payload"]
            elif kind == "day-observation":
                day = message["day"]
                self.observation, self.suggestion = message["observation"], message["suggestion"]
                self.cash[day] = self.observation["cash_microcredits"]
                self.invalid = None
                if day <= self.autopilot_until:
                    self._send({"type": "autopilot", "day": day})
                else:
                    self.pending_day = day
            elif kind == "action-invalid":
                self.invalid = message
            elif kind == "action-sent":
                self.sources[message["day"]] = message["source"]
                self.last_day = message["day"]
                if self.pending_day == message["day"]:
                    self.pending_day = None
            elif kind == "action-receipt":
                payload = message["payload"]
                if not payload["candidate_valid"]:
                    self.receipt_errors[payload["day"]] = payload["errors"]
            elif kind == "action-committed":
                self.committed[message["payload"]["day"]] = message["payload"]
            elif kind == "game-finished":
                self.finished = message["payload"]
            self.cond.notify_all()

    # Driving the seat

    def wait_for_day(self, timeout: float) -> dict | None:
        """The observation awaiting an answer, or None if none came within the timeout or the game is over."""

        deadline = time.monotonic() + timeout
        with self.cond:
            while self.pending_day is None and not self.closed:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                self.cond.wait(remaining)
            return self.observation if self.pending_day is not None else None

    def submit(self, action: dict, timeout: float = 10) -> str | None:
        """Send today's action; None if the bridge sent it, else the error."""

        with self.cond:
            day = self.pending_day
            if day is None:
                return "no day is waiting for an action"
            self._send({"type": "action", "day": day, "action": action})
            deadline = time.monotonic() + timeout
            while self.pending_day == day and self.invalid is None and not self.closed:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return "no answer from the bridge"
                self.cond.wait(remaining)
            if self.invalid is not None and self.invalid["day"] == day:
                error, self.invalid = self.invalid["error"], None
                return error
            return None

    def autopilot(self, days: int, timeout: float) -> int:
        """Let the template play the next `days` days; returns the last day it played."""

        deadline = time.monotonic() + timeout
        with self.cond:
            first = self.pending_day if self.pending_day is not None else self.last_day + 1
            self.autopilot_until = first + days - 1
            if self.pending_day is not None:
                self._send({"type": "autopilot", "day": self.pending_day})
            while self.last_day < self.autopilot_until and not self.closed:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self.cond.wait(remaining)
            return self.last_day
