"""Entry points of the published house agents.

Each public agent id is one executable (installed by `pip install` as a
console script of the same name) that serves one game over stdin / stdout,
exactly as the game runner launches any agent. `python -m
tac_scm_gallery.house --list` lists the ids; `python -m tac_scm_gallery.house
--check-models` verifies the model files against agents/MODELS.md.

Every id is a frozen configuration: the kernels' research overrides
(`TAC_SCM_GALLERY_*` environment variables) are removed before an agent is
built, and the v4 agents load their models from the kit's model directory,
never from an environment-selected file. The model directory is
`$TAC_SCM_HOUSE_MODELS` if set, else `agents/models` next to this package
(an editable install or a source checkout).
"""

from __future__ import annotations

import argparse
from collections.abc import Callable
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import sys

from .agent import GalleryAgent
from .contracts import GalleryKernel
from .runtime import serve_gallery

MODELS_ENVIRONMENT_VARIABLE = "TAC_SCM_HOUSE_MODELS"

# SHA-256 of every model file the v4 agents load (see agents/MODELS.md).
MODEL_FILES = {
    "tactex-v4/models-3.json": "2c523979199cd9153327c4841721996bb09ca6905e306b085c078a32818a4eec",
    "tactex-v4/models-4.json": "243b64397a031b0c5444e212d1594eac288a1212224945dd262da575837ec04d",
    "tactex-v4/models-5.json": "31b65521a2af430aec921d06730e5da8c75bb1be1dbd051f258abe67c6b31ef2",
    "deepmaize-v4-m2/component-residual.json": "ba7bf5d825d0f12ab7d53d943524479d182578e2f240a9ef0672dd109e3da572",
    "deepmaize-v4-m2/pc-database.npz": "d5059386e7fc232700c839bf73be3f212ad266cd0b01559e79b62144935471ca",
}


class ModelsMissing(RuntimeError):
    """A v4 agent's model file is not in the model directory."""


@dataclass(frozen=True)
class Arguments:
    campaign_id: str
    artifact_digest: str
    instance_id: str
    block_index: int


def models_directory() -> Path:
    configured = os.environ.get(MODELS_ENVIRONMENT_VARIABLE)
    if configured:
        return Path(configured)
    return Path(__file__).resolve().parent.parent / "models"


def model_path(relative: str) -> Path:
    path = models_directory() / relative
    if not path.is_file():
        raise ModelsMissing(
            f"model file {path} not found; see agents/MODELS.md (set {MODELS_ENVIRONMENT_VARIABLE} "
            "to the model directory if the agents are not installed in editable mode)"
        )
    return path


# ---- The kernels, one constructor per public id ----


def _redagent(_: Arguments) -> GalleryKernel:
    from .redagent import RedAgentMarketKernel

    return RedAgentMarketKernel(market_enabled=True)


def _deepmaize(_: Arguments) -> GalleryKernel:
    from .deepmaize import DeepMaizeFeedbackKernel

    return DeepMaizeFeedbackKernel(value_feedback=True)


def _packatac(_: Arguments) -> GalleryKernel:
    from .packatac import PackaTacRiskKernel

    return PackaTacRiskKernel(relaxed=False)


def _crocodile(_: Arguments) -> GalleryKernel:
    from .crocodile import CrocodileMixedHorizonKernel

    return CrocodileMixedHorizonKernel(variant="full")


def _phantagent(_: Arguments) -> GalleryKernel:
    from .phantagent import PhantAgentSelectorKernel

    return PhantAgentSelectorKernel(stationary_mode=None)


def _minnetac(_: Arguments) -> GalleryKernel:
    from .minnetac import MinneTacRegimeKernel

    return MinneTacRegimeKernel(stationary_balanced=False)


def _botticelli(_: Arguments) -> GalleryKernel:
    from .botticelli import BotticelliScenarioKernel

    return BotticelliScenarioKernel(mode="scenario")


def _narc(_: Arguments) -> GalleryKernel:
    from .narc import NarcProcurementKernel

    return NarcProcurementKernel(mode="mdp")


def _southampton(_: Arguments) -> GalleryKernel:
    from .southampton import SouthamptonFuzzyKernel

    return SouthamptonFuzzyKernel(mode="fuzzy")


def _tactex(_: Arguments) -> GalleryKernel:
    from .tactex import TacTexPredictiveKernel

    return TacTexPredictiveKernel(mode="full")


def _socrates(arguments: Arguments) -> GalleryKernel:
    from .socrates import SocratesOpponentKernel, public_labels_from_instance

    return SocratesOpponentKernel(mode="full", public_opponent_labels=public_labels_from_instance(arguments.instance_id))


def _mertacor(_: Arguments) -> GalleryKernel:
    from .mertacor import MertacorPolicyKernel

    return MertacorPolicyKernel(mode="policy-search")


def _iota(_: Arguments) -> GalleryKernel:
    from .iota import IotaClassifierKernel

    return IotaClassifierKernel(mode="adaptive")


def _tactex_v4(models: str) -> Callable[[Arguments], GalleryKernel]:
    def build(arguments: Arguments) -> GalleryKernel:
        from .tactex_v4 import TacTexV4Kernel, TacTexV4Parameters
        from .tactex_v4_models import TacTexV4Models

        loaded = TacTexV4Models.load(str(model_path(models)))
        return TacTexV4Kernel(parameters=TacTexV4Parameters(), models=loaded, seed=arguments.block_index)

    return build


def _deepmaize_v4(models: str) -> Callable[[Arguments], GalleryKernel]:
    def build(_: Arguments) -> GalleryKernel:
        from .deepmaize_v3 import DeepMaizeV3Parameters
        from .deepmaize_v4 import DeepMaizeV4Kernel

        model_path(f"{models}/pc-database.npz")
        model_path(f"{models}/component-residual.json")
        return DeepMaizeV4Kernel(parameters=DeepMaizeV3Parameters(), models=models_directory() / models)

    return build


def _botticelli_v4(_: Arguments) -> GalleryKernel:
    from .botticelli_v3 import BotticelliV3Parameters
    from .botticelli_v4 import BotticelliV4Kernel, BotticelliV4Model

    return BotticelliV4Kernel(parameters=BotticelliV3Parameters(), model=BotticelliV4Model())


def _botticelli_v4_1(_: Arguments) -> GalleryKernel:
    from .botticelli_v3 import BotticelliV3Parameters
    from .botticelli_v4 import BotticelliV4Model
    from .botticelli_v4_1 import BotticelliV41Kernel

    return BotticelliV41Kernel(parameters=BotticelliV3Parameters(), model=BotticelliV4Model())


AGENTS: dict[str, Callable[[Arguments], GalleryKernel]] = {
    "redagent-inspired-v1": _redagent,
    "deepmaize-inspired-v1": _deepmaize,
    "packatac-inspired-v1": _packatac,
    "crocodile-inspired-v1": _crocodile,
    "phantagent-inspired-v1": _phantagent,
    "minnetac-inspired-v1": _minnetac,
    "botticelli-inspired-v1": _botticelli,
    "narc-inspired-v1": _narc,
    "southampton-inspired-v1": _southampton,
    "tactex-inspired-v1": _tactex,
    "socrates-inspired-v1": _socrates,
    "mertacor-inspired-v1": _mertacor,
    "iota-inspired-v1": _iota,
    "tactex-inspired-v4-m3": _tactex_v4("tactex-v4/models-3.json"),
    "tactex-inspired-v4-m4": _tactex_v4("tactex-v4/models-4.json"),
    "tactex-inspired-v4-m5": _tactex_v4("tactex-v4/models-5.json"),
    "deepmaize-inspired-v4-m2": _deepmaize_v4("deepmaize-v4-m2"),
    "botticelli-inspired-v4": _botticelli_v4,
    "botticelli-inspired-v4-1": _botticelli_v4_1,
}


def serve(agent_id: str, argv: list[str] | None = None) -> int:
    """Serve one game as the house agent `agent_id`."""

    parser = argparse.ArgumentParser(prog=agent_id)
    parser.add_argument("--campaign-id", default="manual")
    parser.add_argument("--artifact-digest", default="0" * 64)
    parser.add_argument("--instance-id", default=agent_id.split("-", 1)[0])
    parser.add_argument("--block-index", type=int, default=0)
    namespace, _ = parser.parse_known_args(argv)
    arguments = Arguments(namespace.campaign_id, namespace.artifact_digest, namespace.instance_id, namespace.block_index)
    for name in [name for name in os.environ if name.startswith("TAC_SCM_GALLERY_")]:
        del os.environ[name]
    try:
        kernel = AGENTS[agent_id](arguments)
    except ModelsMissing as error:
        print(f"{agent_id}: {error}", file=sys.stderr)
        return 2
    return serve_gallery(GalleryAgent(kernel))


def _entry(agent_id: str) -> Callable[[], int]:
    def main() -> int:
        return serve(agent_id)

    main.__name__ = agent_id.replace("-", "_")
    return main


redagent_inspired_v1 = _entry("redagent-inspired-v1")
deepmaize_inspired_v1 = _entry("deepmaize-inspired-v1")
packatac_inspired_v1 = _entry("packatac-inspired-v1")
crocodile_inspired_v1 = _entry("crocodile-inspired-v1")
phantagent_inspired_v1 = _entry("phantagent-inspired-v1")
minnetac_inspired_v1 = _entry("minnetac-inspired-v1")
botticelli_inspired_v1 = _entry("botticelli-inspired-v1")
narc_inspired_v1 = _entry("narc-inspired-v1")
southampton_inspired_v1 = _entry("southampton-inspired-v1")
tactex_inspired_v1 = _entry("tactex-inspired-v1")
socrates_inspired_v1 = _entry("socrates-inspired-v1")
mertacor_inspired_v1 = _entry("mertacor-inspired-v1")
iota_inspired_v1 = _entry("iota-inspired-v1")
tactex_inspired_v4_m3 = _entry("tactex-inspired-v4-m3")
tactex_inspired_v4_m4 = _entry("tactex-inspired-v4-m4")
tactex_inspired_v4_m5 = _entry("tactex-inspired-v4-m5")
deepmaize_inspired_v4_m2 = _entry("deepmaize-inspired-v4-m2")
botticelli_inspired_v4 = _entry("botticelli-inspired-v4")
botticelli_inspired_v4_1 = _entry("botticelli-inspired-v4-1")


def check_models() -> int:
    """Compare every model file with its recorded SHA-256."""

    failures = 0
    for relative, expected in sorted(MODEL_FILES.items()):
        path = models_directory() / relative
        if not path.is_file():
            print(f"missing  {path}")
            failures += 1
            continue
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1 << 20), b""):
                digest.update(block)
        ok = digest.hexdigest() == expected
        failures += not ok
        print(f"{'ok      ' if ok else 'MISMATCH'} {path}")
    return 1 if failures else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m tac_scm_gallery.house")
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--list", action="store_true", help="list the public agent ids")
    group.add_argument("--check-models", action="store_true", help="verify the model files")
    group.add_argument("--serve", metavar="AGENT_ID", help="serve one game as this agent")
    arguments, rest = parser.parse_known_args(argv)
    if arguments.list:
        print("\n".join(AGENTS))
        return 0
    if arguments.check_models:
        return check_models()
    if arguments.serve not in AGENTS:
        parser.error(f"unknown agent id {arguments.serve}")
    return serve(arguments.serve, rest)


if __name__ == "__main__":
    raise SystemExit(main())
