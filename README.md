# TAC SCM Challenge

A challenge in the Trading Agent Competition Supply Chain
Management game (TAC SCM): build an agent that runs a PC manufacturer for
220 simulated days — winning customer orders, buying components from
suppliers, producing and delivering — and ends with the most money.

**About this repository.** This is a reconstruction of the TAC SCM game as
it was played in the Trading Agent Competition, rebuilt from the published
game specifications for an informal challenge. It makes no claim to
reproduce the original game server or competitions exactly;
`rules/CHANGES.md` lists how the game here differs from the 2007 rules.
The house agents are reconstructions from the published descriptions of
the original competition agents, not those agents themselves, and make no
claim to do their originals justice.

An informal challenge among colleagues. Everything an entrant needs to
prepare is in this kit, the same for every entrant:

| Directory | Contents |
|---|---|
| `rules/` | The game rules as a normative document, with every constant |
| `server/` | The game engine and runner, open source, with conformance tests |
| `sdk/` | The Python SDK, the protocol specification and a template agent |
| `agents/` | House agents inspired by published TAC SCM agents, with their trained models |
| `runner/` | Local games and tournaments under the challenge's resource limits |
| `tournament/` | The format, how to submit and, after the tournament, the results |

## Getting started

Needs git, Rust 1.96 or later (`rustup`), [uv](https://docs.astral.sh/uv/)
(it installs the Python 3.12 pinned in `.python-version`), and Docker (Docker
23 or later) for building a submission image. From a fresh clone to a
submitted agent:

```sh
# 1. The kit at the edition's tag
git clone --branch v1.0 https://github.com/rekrevs/tacscm-challenge.git
cd tacscm-challenge

# 2. The game server
(cd server && cargo build --release --locked)

# 3. The SDK and the house agents (the uv workspace in pyproject.toml, pinned by uv.lock)
uv sync --locked

# 4. The DeepMaize v4 database (210 MB, a release asset; agents/MODELS.md)
curl -fL -o agents/models/deepmaize-v4-m2/pc-database.npz \
  https://github.com/rekrevs/tacscm-challenge/releases/download/v1.0/pc-database.npz
uv run python -m tac_scm_gallery.house --check-models

# 5. A game: the template agent against five house agents (3 to 5 minutes)
uv run python runner/play_game.py --root 0 --out runs/g0 \
  tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 botticelli-inspired-v4-1 \
  narc-inspired-v1 iota-inspired-v1 sdk/template/my_agent.py

# 6. Your agent, built as a submission image and played as the tournament
#    plays it (on an Apple-silicon Mac, leave out --platform for local games)
cp -r sdk/template my-agent        # then write your policy in my-agent/my_agent.py
docker build --platform linux/amd64 -f sdk/template/Dockerfile \
  --build-arg AGENT=my-agent -t my-agent .
uv run python runner/play_game.py --root 1 --out runs/g1 image:my-agent \
  tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 botticelli-inspired-v4-1 \
  iota-inspired-v1 narc-inspired-v1
```

7. **Submit** before the deadline, as `tournament/SUBMISSION.md` section 1
   says: the `linux/amd64` image (a registry reference the organiser can
   pull, or the file from `docker save my-agent | gzip > my-agent.tar.gz`),
   or a zip of your agent directory that the organiser builds with the same
   Dockerfile. Send it to the organiser with its image ID
   (`docker image inspect --format '{{.Id}}' my-agent`); your last
   submission before the deadline is the one that plays.

### Dev container (this fork)

- **Locally:** open the folder in VS Code and *Reopen in Container*.
  `.devcontainer/setup.sh` builds the server, runs `uv sync` and fetches
  the DeepMaize database (steps 2 to 4); Docker is available inside for
  submission images. The image also holds, from Nix, the Haskell agent's
  tools (`haskell-agent/build.nix`'s `tools`): GHC with its dependencies,
  cabal, haskell-language-server, hlint, fourmolu, BNFC, alex and happy.
  The volumes `tacscm-nix` (`/nix`) and `tacscm-cache` (`~/.cache`: cabal,
  HLS, uv) persist across rebuilds; the setup brings the tools up to date.
- **On a Kubernetes cluster:** copy `.env.example` to `.env` (git-ignored)
  and fill in the cluster, namespace, image, volume and size. `ice/devpod.sh
  up` starts the same image as a pod, clones the repo into a folder of the
  volume and runs the setup. Attach VS Code from the Kubernetes view
  (*Attach Visual Studio Code*) or use `ice/devpod.sh shell`;
  `ice/devpod.sh down` stops it and the checkout stays on the volume.
  `.github/workflows/devcontainer-image.yml` publishes the image to
  `ghcr.io/<owner>/<repo>-devcontainer`.
- **Haskell agent:** `haskell-agent/scripts/run-tournament.sh [--pod |
  --docker] [-f FIELD] [-r "ROOTS"] [AGENT]` builds `haskell-agent/`
  (running its tests) and plays `tacscm-hs-AGENT` (default `tracker`) in a
  paired set against a house field of `tournament/FORMAT.md` (default
  `primary`, root 0), here, in the dev pod or in a dev container image.
  `haskell-agent/scripts/plot.py` serves live plots of the games under `runs/`.
- **Commit checks:** `git config core.hooksPath .githooks` once per clone
  (the container setup does it) blocks commits containing secrets or `.env`.

Where to read next: `sdk/README.md` (writing an agent), `rules/` (the game),
`tournament/SUBMISSION.md` (limits per seat), `tournament/FORMAT.md` (how
the tournament is played and scored), `runner/README.md` (local games and
paired sets).

## How the challenge works

- You develop and train your agent on your own machine against any
  combination of the house agents (and copies of your own agent).
- You submit it as a container image built from the template in `sdk/`
  (or a zip we build for you). Submitted agents run on the same hardware
  under the same limits, with no network access and no state carried
  between games.
- Agents are frozen at the deadline. The ranking comes from games with
  all entrants together, filled with house agents; every entrant also
  plays a published list of house-agent fields, reported separately.
  Everyone plays the same game seeds with seats rotated; the seeds are
  drawn at random after the deadline and published with the results
  (`tournament/FORMAT.md`).
- Results come as a table with the seeds and each entrant's image ID; game
  logs on request.

## Status

Edition 1, kit tag **`v1.0`**, frozen: the server, the rules, the house
agents and their models, the seat limits and `tournament/FORMAT.md` stay as
tagged for this edition; a fix found during the edition goes into the next
one. Game profile `tac-scm-2026-draft3` (profile revision 1), engine
`tac-scm-engine-core` 0.5.1 (`rules/PROFILE.md`, `server/README.md`).

## Licence

MIT, see `LICENSE`.
