#!/usr/bin/env python3
"""Process-level fallback for the tournament's seat limits, when Docker is not
available.

    python runner/limit.py --cpus 2 --memory-mib 8192 --report seat.json -- EXE [ARG...]

Runs EXE with this process's stdin and stdout (the game protocol passes
straight through) and approximates the container limits of
tournament/SUBMISSION.md:

- memory: the resident memory of EXE and all its descendants is sampled
  every 50 ms (psutil if installed, else `ps`); above the limit the whole
  process group is killed, as the container's OOM killer would;
- cores: OMP_NUM_THREADS and the other common thread-pool variables are set
  to --cpus; nothing caps the CPU time actually used;
- network: denied with `sandbox-exec` on macOS or `unshare --net` on Linux
  where these work; otherwise left open and said so in the report.

This is NOT the tournament's sandbox: a container's memory limit also counts
page cache and is enforced by the kernel at once, its CPU limit is a hard
quota, and its network is absent. Use Docker (`image:` seats in
play_game.py) to reproduce tournament conditions; use this to catch an agent
that is far over the memory limit or needs the network.

The report (JSON) records wall time, CPU time of the agent's process tree,
the sampled peak resident memory, whether the agent was killed for memory,
and how the network was handled.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import subprocess
import sys
import threading
import time

THREAD_VARIABLES = (
    "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "NUMEXPR_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS", "BLIS_NUM_THREADS", "RAYON_NUM_THREADS",
)
SANDBOX_PROFILE = "(version 1)(allow default)(deny network-outbound (remote ip))(deny network-inbound (local ip))"


def network_prefix() -> tuple[list[str], str]:
    """A command prefix that removes network access, and how it does so."""

    if sys.platform == "darwin" and shutil.which("sandbox-exec"):
        probe = subprocess.run(["sandbox-exec", "-p", SANDBOX_PROFILE, "true"], capture_output=True, check=False)
        if probe.returncode == 0:
            return ["sandbox-exec", "-p", SANDBOX_PROFILE], "denied (sandbox-exec)"
        return [], "not isolated (sandbox-exec unavailable here)"
    if sys.platform.startswith("linux") and shutil.which("unshare"):
        probe = subprocess.run(["unshare", "--net", "--map-root-user", "true"], capture_output=True, check=False)
        if probe.returncode == 0:
            return ["unshare", "--net", "--map-root-user"], "denied (unshare --net)"
        return [], "not isolated (unshare --net not permitted)"
    return [], "not isolated (no sandbox-exec or unshare)"


def tree_rss_bytes(root: int) -> int:
    """Resident memory of `root` and all its descendants."""

    try:
        import psutil  # type: ignore[import-not-found]
    except ImportError:
        psutil = None
    if psutil is not None:
        try:
            process = psutil.Process(root)
            total = process.memory_info().rss
            for child in process.children(recursive=True):
                try:
                    total += child.memory_info().rss
                except psutil.Error:
                    pass
            return total
        except psutil.Error:
            return 0
    listing = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss="], capture_output=True, text=True, check=False).stdout
    children: dict[int, list[int]] = {}
    rss: dict[int, int] = {}
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) == 3:
            pid, ppid, kib = (int(field) for field in fields)
            children.setdefault(ppid, []).append(pid)
            rss[pid] = kib * 1024
    total, stack = 0, [root]
    while stack:
        pid = stack.pop()
        total += rss.get(pid, 0)
        stack.extend(children.get(pid, ()))
    return total


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cpus", type=int, required=True)
    parser.add_argument("--memory-mib", type=int, required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--allow-network", action="store_true", help="do not try to remove network access")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    arguments = parser.parse_args(argv)
    command = arguments.command[1:] if arguments.command[:1] == ["--"] else arguments.command
    if not command:
        parser.error("no command given")

    environment = dict(os.environ)
    environment.update({name: str(arguments.cpus) for name in THREAD_VARIABLES})
    prefix, network = ([], "allowed (--allow-network)") if arguments.allow_network else network_prefix()
    limit = arguments.memory_mib * 1024 * 1024
    started = time.monotonic()
    child = subprocess.Popen([*prefix, *command], env=environment, start_new_session=True)
    state = {"peak": 0, "killed": None}
    finished = threading.Event()

    def watch() -> None:
        interval = 0.05
        while not finished.wait(interval):
            used = tree_rss_bytes(child.pid)
            state["peak"] = max(state["peak"], used)
            if used > limit:
                state["killed"] = "memory"
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                return

    def forward(signum: int, _frame: object) -> None:
        try:
            os.killpg(child.pid, signum)
        except ProcessLookupError:
            pass

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, forward)
    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    code = child.wait()
    finished.set()
    watcher.join(timeout=1)
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    maxrss = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
    report = {
        "fallback": "process-level (not the tournament container)",
        "command": command,
        "cpus": arguments.cpus,
        "memory_mib": arguments.memory_mib,
        "network": network,
        "exit_code": code,
        "killed": state["killed"],
        "wall_s": round(time.monotonic() - started, 2),
        "cpu_s": round(usage.ru_utime + usage.ru_stime, 2),
        "peak_rss_mib": round(max(state["peak"], maxrss) / 2**20, 1),
        "over_limit": max(state["peak"], maxrss) > limit,
    }
    if arguments.report:
        arguments.report.write_text(json.dumps(report, indent=2) + "\n")
    if state["killed"] or report["over_limit"]:
        print(f"limit.py: {command[0]} {'killed' if state['killed'] else 'went'} above {arguments.memory_mib} MiB",
              file=sys.stderr)
    return 137 if state["killed"] else (code if code >= 0 else 128 - code)


if __name__ == "__main__":
    raise SystemExit(main())
