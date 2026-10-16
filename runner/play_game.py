#!/usr/bin/env python3
"""Play one local game of six agents under the challenge profile and limits.

    python runner/play_game.py --root 0 --out runs/game-0 \\
        tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 botticelli-inspired-v4 \\
        narc-inspired-v1 iota-inspired-v1 image:my-agent

Each AGENT is one of:

- `image:REF`: a submission image (tournament/SUBMISSION.md), run as
  `docker run -i --network none --cpus 2 --memory 8g --memory-swap 8g
  -e OMP_NUM_THREADS=2 -e OPENBLAS_NUM_THREADS=2 -e MKL_NUM_THREADS=2
  --entrypoint /agent/run REF`, the tournament's seat; needs a Docker daemon
  and the image present locally (built or pulled);
- a house agent id or baseline installed in the active Python environment,
  any command on PATH, or a path to an executable (for example
  `sdk/template/run`): run as a local process, with no limits unless
  `--process-limits` is given.

`--process-limits` runs every local seat under `runner/limit.py`, a
process-level approximation of the seat limits for machines without Docker
(memory watchdog, thread-pool caps, network denied where the OS allows); it
is labelled as such in the output and is not the tournament's container.

Seats follow the order given (agent-a .. agent-f). The runner sets the profile
`tac-scm-2026-draft3`, the 15 s daily action deadline and the challenge's
startup grace (rules/TIMING.md, tournament/SUBMISSION.md). The root seed is
SHA-256 of `tacscm-challenge/local/<root>` unless --seed gives one. Scores are
printed in M credits and written with the per-seat defaults, slowest decision
and (for limited seats) memory peaks to `<out>/result.json`; the runner's own
game files are in `<out>/game/`.

Needs the SDK installed (it reads the runner's CBOR result records).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

PROFILE = "tac-scm-2026-draft3"
ACTION_DEADLINE_MS = 15_000
SHUTDOWN_GRACE_MS = 5_000
# The tournament's seat class (tournament/SUBMISSION.md): requests = limits.
SEAT_CPUS = 2
SEAT_MEMORY_MIB = 8 * 1024
STARTUP_GRACE_MS = 30_000
AGENT_COMMAND = "/agent/run"
# Set in every seat to the seat's cores: inside a container the OS reports
# the whole machine, and thread pools sized to it run slower under the quota.
THREAD_VARIABLES = ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS")
KIT = Path(__file__).resolve().parent.parent
DEFAULT_SERVER = KIT / "server" / "target" / "release" / "tac-scm-game-runner"
SEATS = tuple(f"agent-{letter}" for letter in "abcdef")


def resolve_agent(name: str) -> Path:
    candidate = Path(name)
    if candidate.is_file():
        return candidate.resolve()
    search = os.pathsep.join([str(Path(sys.executable).parent), os.environ.get("PATH", "")])
    found = shutil.which(name, path=search)
    if found is None:
        raise SystemExit(f"agent {name!r} is neither a file nor an executable on PATH")
    return Path(found).resolve()


def instance_id(seat: int, name: str) -> str:
    stem = "".join(ch if ch.isalnum() or ch in "._-" else "-" for ch in Path(name).name.lower())
    return f"{'abcdef'[seat]}-{stem}"[:32]


def docker_available() -> bool:
    if shutil.which("docker") is None:
        return False
    return subprocess.run(["docker", "info"], capture_output=True, check=False).returncode == 0


def write_executable(path: Path, text: str) -> Path:
    path.write_text(text)
    path.chmod(0o755)
    return path


def prepare_seat(index: int, spec: str, arguments: argparse.Namespace, seats_dir: Path, tag: str) -> dict:
    """The executable the runner launches for one seat, and what it runs."""

    seat = SEATS[index]
    if spec.startswith("image:"):
        reference = spec.removeprefix("image:")
        if not docker_available():
            raise SystemExit(
                f"{spec}: no Docker daemon. Run the agent directory as a local executable instead, "
                "with --process-limits for the process-level fallback (runner/README.md).")
        inspected = subprocess.run(
            ["docker", "image", "inspect", "--format", "{{.Id}} {{.Architecture}} {{json .RepoDigests}}", reference],
            capture_output=True, text=True, check=False)
        if inspected.returncode != 0:
            raise SystemExit(f"{spec}: image not found locally; build or pull it first")
        image_id, architecture, digests = inspected.stdout.strip().split(" ", 2)
        container = f"tacscm-{tag}-{seat}"
        command = [
            "docker", "run", "-i", "--name", container, "--network", "none",
            "--cpus", str(arguments.cpus), "--memory", f"{arguments.memory_mib}m",
            "--memory-swap", f"{arguments.memory_mib}m", "--pull", "never",
            *(f"--env={name}={arguments.cpus}" for name in THREAD_VARIABLES),
            "--entrypoint", AGENT_COMMAND, image_id,
        ]
        executable = write_executable(seats_dir / seat, "#!/bin/sh\nexec " + " ".join(command) + "\n")
        return {"seat": seat, "agent": spec, "kind": "container", "executable": executable,
                "image_id": image_id, "architecture": architecture, "repo_digests": json.loads(digests),
                "container": container}
    path = resolve_agent(spec)
    row = {"seat": seat, "agent": spec, "kind": "process", "executable": path,
           "executable_sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
    if arguments.process_limits:
        report = seats_dir / f"{seat}.limits.json"
        command = [sys.executable, str(KIT / "runner" / "limit.py"), "--cpus", str(arguments.cpus),
                   "--memory-mib", str(arguments.memory_mib), "--report", str(report), "--", str(path)]
        row.update(kind="process-limited (fallback)", report=report,
                   executable=write_executable(seats_dir / seat, "#!/bin/sh\nexec " + " ".join(
                       f"'{part}'" for part in command) + "\n"))
    return row


def finish_container(row: dict) -> None:
    """Record whether the seat's container was OOM-killed, then remove it."""

    state = subprocess.run(["docker", "inspect", "--format", "{{.State.OOMKilled}} {{.State.ExitCode}}", row["container"]],
                           capture_output=True, text=True, check=False)
    if state.returncode == 0:
        oom, code = state.stdout.split()
        row["oom_killed"] = oom == "true"
        row["exit_code"] = int(code)
    subprocess.run(["docker", "rm", "-f", row["container"]], capture_output=True, check=False)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("agents", nargs=6, metavar="AGENT")
    parser.add_argument("--out", type=Path, required=True, help="new output directory")
    parser.add_argument("--root", type=int, default=0, help="local root index (default 0)")
    parser.add_argument("--seed", help="explicit 64-hex-digit root seed instead of --root")
    parser.add_argument("--server", type=Path, default=DEFAULT_SERVER, help="tac-scm-game-runner binary")
    parser.add_argument("--process-limits", action="store_true",
                        help="run local seats under runner/limit.py (process-level fallback, not the container)")
    parser.add_argument("--cpus", type=int, default=SEAT_CPUS, help=f"cores per seat (tournament: {SEAT_CPUS})")
    parser.add_argument("--memory-mib", type=int, default=SEAT_MEMORY_MIB,
                        help=f"memory per seat in MiB (tournament: {SEAT_MEMORY_MIB})")
    parser.add_argument("--startup-grace-ms", type=int, default=STARTUP_GRACE_MS,
                        help=f"wait after start before day 0 (tournament: {STARTUP_GRACE_MS})")
    parser.add_argument("--training-record", action="store_true",
                        help="write every seat's daily observation and action to <out>/training.cbor")
    parser.add_argument("--deadline-ms", type=int, default=ACTION_DEADLINE_MS, help=argparse.SUPPRESS)
    arguments = parser.parse_args(argv)

    from tac_scm_sdk import cbor

    if not arguments.server.is_file():
        raise SystemExit(f"game runner {arguments.server} not found; build it (server/README.md)")
    seed = arguments.seed or hashlib.sha256(f"tacscm-challenge/local/{arguments.root}".encode()).hexdigest()
    if len(seed) != 64 or any(ch not in "0123456789abcdef" for ch in seed):
        raise SystemExit("--seed must be 64 lowercase hexadecimal digits")
    if arguments.out.exists():
        raise SystemExit(f"{arguments.out} exists; choose a new output directory")
    arguments.out.mkdir(parents=True)
    game = arguments.out / "game"
    seats_dir = arguments.out / "seats"
    seats_dir.mkdir()
    tag = hashlib.sha256(str(arguments.out.resolve()).encode() + str(time.time()).encode()).hexdigest()[:10]
    rows = [prepare_seat(index, spec, arguments, seats_dir, tag) for index, spec in enumerate(arguments.agents)]
    (seats_dir / "agents.json").write_text(json.dumps({row["seat"]: row["agent"] for row in rows}, indent=2) + "\n")
    campaign = "local"
    command = [
        str(arguments.server), "planned", str(game), seed, campaign,
        hashlib.sha256(campaign.encode()).hexdigest(), str(arguments.root), "local", "0", str(arguments.root),
        *(f"{instance_id(index, row['agent'])}={row['executable']}" for index, row in enumerate(rows)),
    ]
    environment = dict(os.environ)
    environment.update({
        "TAC_SCM_PROFILE": PROFILE,
        "TAC_SCM_ACTION_DEADLINE_MS": str(arguments.deadline_ms),
        "TAC_SCM_STARTUP_GRACE_MS": str(arguments.startup_grace_ms),
        "TAC_SCM_SHUTDOWN_GRACE_MS": str(SHUTDOWN_GRACE_MS),
        "TAC_SCM_EVIDENCE": "thin",
        "TAC_SCM_RUNTIME_SUMMARY": "1",
    })
    if arguments.training_record:
        environment["TAC_SCM_TRAINING_RECORD"] = str(arguments.out / "training.cbor")
    # Agents run with the Python environment of this script first on PATH.
    environment["PATH"] = os.pathsep.join([str(Path(sys.executable).parent), environment.get("PATH", "")])
    started = time.monotonic()
    try:
        completed = subprocess.run(command, env=environment, capture_output=True, check=False)
    finally:
        for row in rows:
            if row["kind"] == "container":
                finish_container(row)
    wall = time.monotonic() - started
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr.decode("utf-8", "replace")[-4000:])
        raise SystemExit(f"game runner failed with exit code {completed.returncode}")
    if PROFILE.encode() not in (game / "game-plan.cbor").read_bytes():
        raise SystemExit(f"game plan does not name profile {PROFILE}")
    record = cbor.loads((game / "result-record.cbor").read_bytes())
    runtime = cbor.loads((game / "runtime-summary.cbor").read_bytes())
    seats = []
    for index, row in enumerate(rows):
        entry = {key: (str(value) if isinstance(value, Path) else value)
                 for key, value in row.items() if key not in ("report",)}
        entry.update({
            "score_microcredits": int(record["scores_microcredits"][index]),
            "default_days": int(record["default_counts"][index]),
            "max_decision_s": round(runtime["decision_max_ns"][index] / 1e9, 2),
        })
        if "report" in row and row["report"].is_file():
            entry["limits"] = json.loads(row["report"].read_text())
        seats.append(entry)
    result = {
        "profile": PROFILE,
        "root": arguments.root if arguments.seed is None else None,
        "root_seed": seed,
        "termination": record["termination"],
        "seat_class": {"cpus": arguments.cpus, "memory_mib": arguments.memory_mib,
                       "startup_grace_ms": arguments.startup_grace_ms},
        "wall_s": round(wall, 1),
        "seats": seats,
    }
    (arguments.out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"profile {PROFILE}  root seed {seed[:16]}...  {record['termination']}  {wall:.0f} s")
    print(f"{'seat':8} {'agent':28} {'score (M)':>10} {'defaults':>8} {'max s':>6}  limits")
    for row in seats:
        note = ""
        if row["kind"] == "container":
            note = f"container {arguments.cpus} cpu {arguments.memory_mib} MiB" + (", OOM-KILLED" if row.get("oom_killed") else "")
        elif "limits" in row:
            limits = row["limits"]
            note = (f"fallback: peak {limits['peak_rss_mib']:.0f} MiB, cpu {limits['cpu_s']:.0f} s, "
                    f"network {limits['network'].split(' ')[0]}" + (", KILLED (memory)" if limits["killed"] else ""))
        print(f"{row['seat']:8} {row['agent'][:28]:28} {row['score_microcredits'] / 1e12:10.2f} "
              f"{row['default_days']:8d} {row['max_decision_s']:6.2f}  {note}")
    if any(row["kind"].startswith("process-limited") for row in seats):
        print("process-level fallback: memory watchdog and thread caps, not the tournament's container limits")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
