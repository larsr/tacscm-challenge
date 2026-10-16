# Runner

`play_game.py` plays one local game of six agents under the challenge's
profile (`tac-scm-2026-draft3`), daily deadline (15 s) and startup grace
(30 s), and prints each seat's final cash in M credits, its default days and
its slowest decision. `paired_set.py` plays a small paired set: every root
with the six agents rotated through all six seats.

```sh
uv run python runner/play_game.py --root 3 --out runs/g3 \
  tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 botticelli-inspired-v4-1 \
  narc-inspired-v1 iota-inspired-v1 image:my-agent

uv run python runner/paired_set.py --out runs/set1 --roots 0 1 2 \
  image:my-agent tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 \
  botticelli-inspired-v4-1 iota-inspired-v1 narc-inspired-v1
```

## Agents

- `image:REF` is a submission image (`tournament/SUBMISSION.md`), run as
  the tournament runs it: `docker run -i --network none --cpus 2 --memory
  8g --memory-swap 8g --entrypoint /agent/run REF` with `OMP_NUM_THREADS`,
  `OPENBLAS_NUM_THREADS` and `MKL_NUM_THREADS` set to 2, one fresh
  container per game. Needs a Docker daemon and the image present
  locally. If the seat's container is killed for memory, the output says
  `OOM-KILLED`. On an Apple-silicon Mac a `linux/amd64` image runs
  emulated and much slower; for local games build the same Dockerfile
  without `--platform`, and submit the `linux/amd64` build. Six image
  seats can take up to 6 x 8 GiB = 48 GiB and 12 cores of the Docker VM
  (Docker Desktop, OrbStack and colima cap the VM); give it enough, or
  play fewer image seats. Building with `sdk/template/Dockerfile.dockerignore`
  needs BuildKit (Docker 23 or later).
- Anything else is a local executable: a house agent id or baseline
  installed in the active Python environment, a command on PATH, or a path
  (`sdk/template/run`). It runs as a plain process, without limits.
- `--process-limits` runs every local seat under `limit.py`, the
  **process-level fallback** for machines without Docker: the agent's
  process tree is killed when its resident memory exceeds the seat's
  memory, thread-pool variables (`OMP_NUM_THREADS` and the like) are set to
  the seat's cores, and the network is denied with `sandbox-exec` (macOS) or
  `unshare --net` (Linux) where the OS allows. The output prints each seat's
  memory peak and CPU time. It is not the tournament's container: nothing
  caps the CPU actually used, memory is sampled every 50 ms rather than
  enforced by the kernel, and page cache is not counted. Check an agent
  under Docker before you submit it if you can.

`--cpus`, `--memory-mib` and `--startup-grace-ms` change the seat class for
experiments; the defaults are the tournament's. `--training-record` writes
every seat's daily observation and action to `<out>/training.cbor` (both
scripts), as the game is played.

## Output

- `play_game.py`: `<out>/seats/agents.json` (each seat's agent, written when
  the game starts), `<out>/result.json` (scores, defaults, slowest decision,
  the image id or executable hash of every seat, the fallback's memory and
  CPU report per seat) and the runner's own files (game plan, action
  transcript, result record) in `<out>/game/`.
- `paired_set.py`: one `play_game.py` directory per game
  (`<out>/r<root>-s<rotation>/`), and `<out>/report.json` with each agent's
  mean final cash and its standard error clustered by root; `--compare A B`
  adds the paired difference of two agents. An interrupted set resumes
  where it stopped.

`--root N` picks the game's random root: the same root gives the same
demand, supplier capacities and rates. Needs the server built
(`server/README.md`) and the SDK installed.

A game takes under a minute with the v1 house agents and 3 to 5 minutes with
the v4 agents on a laptop, plus the 30 s startup grace. Under tournament
conditions a game needs 12 cores (six seats of 2); run games in parallel
only if the machine has the cores, or decisions slow down. On the
tournament's machines a game in a strong field takes about 20 minutes: a
day closes when the slowest agent has submitted, and
`botticelli-inspired-v4-1` takes about 5 s per day there; with an agent
that uses most of its 15 s every day, about 35 to 40 minutes. A game of
v1 agents takes about 2 minutes. The times include the runner's own
check after the last day, which replays the game (a few minutes when the
agents placed many orders).

## Tests

`uv sync` installs pytest with the `dev` dependency group:

```sh
uv run pytest
```
