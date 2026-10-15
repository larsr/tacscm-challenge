"""Process entry points for the three baseline policies.

Each policy is one executable (`baseline-random`, `baseline-heuristic`,
`baseline-optimizer` after installing the SDK) that serves one game over
stdin / stdout. The game runner may pass the scheduling arguments below; the
random baseline derives its per-day randomness from them and its fixed seed.
"""

from __future__ import annotations

import argparse

from tac_scm_sdk.contracts import Agent
from tac_scm_sdk.runtime import serve

from .baselines import HeuristicAgentV2, LegalRandomAgentV2, RollingHorizonAgentV2

# Fixed internal seeds of the published baseline-v2 artifacts.
SEEDS = {
    "baseline-random-v2": "bfc9d02023d4640f81eac3550fbad930a11cd41f357db8aff3cbbf673af1beaf",
    "baseline-heuristic-v2": "d512da1cf910a8c1bf065407c753a73bc861add031e955a492b4a9455fbab9cf",
    "baseline-optimizer-v2": "849c370e9caa3d421b1ff54856f9a82606309029dcff29b2db211a9a9df3a7a9",
}


def _run(artifact_id: str, argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog=artifact_id)
    parser.add_argument("--campaign-id", default="manual-batch-v2")
    parser.add_argument("--artifact-digest", default=artifact_id)
    parser.add_argument("--instance-id", default="manual-instance")
    parser.add_argument("--block-index", type=int, default=0)
    arguments, _ = parser.parse_known_args(argv)
    context = (arguments.campaign_id, arguments.artifact_digest, arguments.instance_id, arguments.block_index)
    agent: Agent
    if artifact_id == "baseline-random-v2":
        agent = LegalRandomAgentV2(bytes.fromhex(SEEDS[artifact_id]), context)
    elif artifact_id == "baseline-heuristic-v2":
        agent = HeuristicAgentV2()
    elif artifact_id == "baseline-optimizer-v2":
        agent = RollingHorizonAgentV2()
    else:
        raise ValueError(f"unknown baseline: {artifact_id}")
    return serve(agent)


def random_main(argv: list[str] | None = None) -> int:
    return _run("baseline-random-v2", argv)


def heuristic_main(argv: list[str] | None = None) -> int:
    return _run("baseline-heuristic-v2", argv)


def optimizer_main(argv: list[str] | None = None) -> int:
    return _run("baseline-optimizer-v2", argv)
