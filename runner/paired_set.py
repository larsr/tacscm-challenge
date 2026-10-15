#!/usr/bin/env python3
"""Play a small paired set of games and report each agent's mean score.

    python runner/paired_set.py --out runs/set1 --roots 0 1 2 \\
        image:my-agent tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 \\
        botticelli-inspired-v4 iota-inspired-v1 narc-inspired-v1

Every root is played with the six agents rotated through all six seats (six
games per root; `--no-rotate` plays one game per root in the order given), so
every agent meets the same demand and supplier draws from every seat, as in
the tournament (tournament/FORMAT.md). As there, the order the rotations start
from is drawn per root from the root seed (FORMAT.md section 3: the agents as
given are the base order), so the agents' cyclic neighbours change from root
to root; the report shows each root's order. AGENT forms and the options --process-limits,
--cpus, --memory-mib, --startup-grace-ms and --server are those of
play_game.py; each game is played by play_game.py into
`<out>/r<root>-s<rotation>/`.

The report gives per agent: games, mean final cash in M credits, its standard
error clustered by root (the per-root means are the units), mean default
days and the slowest decision. With `--compare A B` it also gives the paired
difference A - B per root (both agents in every game), with its standard
error. A set already partly played is resumed: games with a result.json are
not replayed.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import math
from pathlib import Path
import shutil
import statistics
import subprocess
import sys

HERE = Path(__file__).resolve().parent


def mean_se(values: list[float]) -> tuple[float, float]:
    mean = statistics.fmean(values)
    se = statistics.stdev(values) / math.sqrt(len(values)) if len(values) > 1 else float("nan")
    return mean, se


def root_seed(root: int) -> str:
    """play_game.py's seed for a local root index."""
    return hashlib.sha256(f"tacscm-challenge/local/{root}".encode()).hexdigest()


def listed_order(seed: str, base: list[str]) -> list[str]:
    """tournament/FORMAT.md section 3: the base positions sorted by
    SHA-256(root + "/listed/" + position)."""
    return [base[p] for p in sorted(range(len(base)),
                                    key=lambda p: hashlib.sha256(f"{seed}/listed/{p}".encode()).hexdigest())]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("agents", nargs=6, metavar="AGENT")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--roots", type=int, nargs="+", required=True, help="local root indices")
    parser.add_argument("--no-rotate", action="store_true", help="one game per root, seats as given")
    parser.add_argument("--parallel", type=int, default=1,
                        help="games at once (each needs 6 x the seat's cores to run under tournament conditions)")
    parser.add_argument("--compare", nargs=2, metavar=("A", "B"), help="two of the AGENTs to compare pairwise")
    parser.add_argument("--process-limits", action="store_true", help="as in play_game.py")
    parser.add_argument("--cpus", type=int, help="as in play_game.py")
    parser.add_argument("--memory-mib", type=int, help="as in play_game.py")
    parser.add_argument("--startup-grace-ms", type=int, help="as in play_game.py")
    parser.add_argument("--server", type=Path, help="as in play_game.py")
    arguments = parser.parse_args(argv)
    passthrough = ["--process-limits"] if arguments.process_limits else []
    for option in ("cpus", "memory_mib", "startup_grace_ms", "server"):
        value = getattr(arguments, option)
        if value is not None:
            passthrough += [f"--{option.replace('_', '-')}", str(value)]
    if arguments.compare and not set(arguments.compare) <= set(arguments.agents):
        raise SystemExit("--compare names must be among the AGENTs")
    arguments.out.mkdir(parents=True, exist_ok=True)
    roots = list(dict.fromkeys(arguments.roots))  # a repeated root is one root
    rotations = [0] if arguments.no_rotate else list(range(6))
    games = [(root, rotation) for root in roots for rotation in rotations]
    listed = {root: list(arguments.agents) if arguments.no_rotate else listed_order(root_seed(root), arguments.agents)
              for root in roots}

    def play(game: tuple[int, int]) -> dict:
        root, rotation = game
        directory = arguments.out / f"r{root}-s{rotation}"
        if (directory / "result.json").is_file():
            return json.loads((directory / "result.json").read_text())
        if directory.exists():
            shutil.rmtree(directory)  # an interrupted game of this set; replay it
        seats = listed[root][rotation:] + listed[root][:rotation]
        command = [sys.executable, str(HERE / "play_game.py"), "--root", str(root), "--out", str(directory),
                   *passthrough, *seats]
        completed = subprocess.run(command, capture_output=True, text=True, check=False)
        if completed.returncode != 0:
            raise SystemExit(f"game r{root}-s{rotation} failed:\n{completed.stdout[-2000:]}{completed.stderr[-2000:]}")
        print(f"r{root}-s{rotation} done", flush=True)
        return json.loads((directory / "result.json").read_text())

    with ThreadPoolExecutor(max_workers=max(1, arguments.parallel)) as pool:
        results = dict(zip(games, pool.map(play, games)))

    # Per agent and root: mean score over its seats (an agent listed twice
    # contributes both copies).
    per_root: dict[str, dict[int, list[float]]] = {}
    defaults: dict[str, list[int]] = {}
    slowest: dict[str, float] = {}
    kinds: dict[str, set[str]] = {}
    for (root, _), result in results.items():
        for seat in result["seats"]:
            name = seat["agent"]
            kinds.setdefault(name, set()).add(seat.get("kind", "process"))
            per_root.setdefault(name, {}).setdefault(root, []).append(seat["score_microcredits"] / 1e12)
            defaults.setdefault(name, []).append(seat["default_days"])
            slowest[name] = max(slowest.get(name, 0.0), seat["max_decision_s"])
    report = {"roots": roots, "rotated": not arguments.no_rotate, "games": len(results),
              "listed": {str(root): order for root, order in listed.items()},
              "seat_class": next(iter(results.values())).get("seat_class"), "agents": {}}
    print(f"{len(results)} games on roots {roots}; mean final cash in M, SE clustered by root")
    if not arguments.no_rotate:
        for root, order in listed.items():
            print(f"root {root} listed order: {' '.join(order)}")
    print(f"{'agent':32} {'games':>5} {'mean':>8} {'SE':>6} {'defaults':>8} {'max s':>6}")
    for name in dict.fromkeys(arguments.agents):
        root_means = [statistics.fmean(scores) for scores in per_root[name].values()]
        mean, se = mean_se(root_means)
        count = sum(len(scores) for scores in per_root[name].values())
        report["agents"][name] = {"kind": " / ".join(sorted(kinds[name])), "games": count, "mean_m": mean, "se_m": se,
                                  "mean_default_days": statistics.fmean(defaults[name]), "max_decision_s": slowest[name]}
        print(f"{name[:32]:32} {count:5d} {mean:8.2f} {se:6.2f} {statistics.fmean(defaults[name]):8.1f} {slowest[name]:6.2f}")
    if arguments.compare:
        first, second = arguments.compare
        differences = [statistics.fmean(per_root[first][root]) - statistics.fmean(per_root[second][root])
                       for root in per_root[first]]
        mean, se = mean_se(differences)
        report["compare"] = {"a": first, "b": second, "mean_difference_m": mean, "se_m": se, "roots": len(differences)}
        print(f"paired difference {first} - {second}: {mean:+.2f} M (SE {se:.2f}, {len(differences)} roots)")
    if any(kind.startswith("process-limited") for names in kinds.values() for kind in names):
        report["note"] = "process-level fallback: memory watchdog and thread caps, not the tournament's container limits"
        print(report["note"])
    (arguments.out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
