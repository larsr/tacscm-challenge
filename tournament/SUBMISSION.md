# Submitting an agent

How to package an agent, the limits it plays under, and what happens when
something goes wrong. The game's own timing (the 15 s daily deadline,
defaults, what counts as malformed) is in `rules/TIMING.md`; where that
document refers to the challenge's values, they are the ones below.

## 1. What you submit

Either of:

- **An image** built from `sdk/template/Dockerfile` (or any image that
  meets section 2), for `linux/amd64`. Push it to a registry the organiser
  can pull from and send the reference, or send the file from
  `docker save my-agent | gzip > my-agent.tar.gz`.
- **A zip of your agent directory**, laid out like `sdk/template/`: an
  executable `run` that starts the agent, your code and data, and
  optionally a `requirements.txt` with every version pinned
  (`pip freeze`). The organiser builds it with `sdk/template/Dockerfile`
  unchanged and sends you the image digest to confirm; build it yourself
  the same way first to be sure it works.

Build from the root of the kit:

```sh
cp -r sdk/template my-agent        # then edit my-agent/my_agent.py
docker build --platform linux/amd64 -f sdk/template/Dockerfile \
  --build-arg AGENT=my-agent -t my-agent .
uv run python runner/play_game.py --out runs/t1 image:my-agent \
  tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 botticelli-inspired-v4-1 \
  iota-inspired-v1 narc-inspired-v1
```

A submission is identified by its **image ID**, the digest of the image's
configuration (`docker image inspect --format '{{.Id}}' my-agent`). It is
the same for a pushed image and for a `docker save` file, whereas the
registry (repo) digest depends on the registry and compression and is
absent for a loaded file; the results record both. At the deadline the image
ID of each entrant's last submission is recorded and frozen; that image
plays every game. The organiser records his own agent's image ID before he
opens any submission.

`sdk/template/Dockerfile.dockerignore` keeps the large local build products
out of the build context; it needs BuildKit (Docker 23 or later, the
default there). An older Docker ignores it and sends the whole kit as
context.

## 2. What the image must do

- `/agent/run` exists and is executable; started with no arguments it
  speaks the game protocol (`sdk/PROTOCOL.md`) on stdin and stdout until
  stdin closes. The template's `run` does `exec python .../my_agent.py`.
- The image has `/bin/sh` (every Debian- or Alpine-based image does; the
  tournament starts the seat through a small shell step).
- Anything written to stderr has no effect on the game; the organiser
  can send you your agent's stderr from a game on request.
- It needs nothing from outside: no network, no mounted files. The only
  environment variables set beyond the image's own are
  `OMP_NUM_THREADS`, `OPENBLAS_NUM_THREADS` and `MKL_NUM_THREADS`, each
  equal to the seat's cores (2). Models and precomputed data go into the
  image.
- **No state across games.** Every game starts a fresh container from the
  frozen image; whatever it writes is discarded.
- **The SDK stops your agent on an action it considers invalid.** A
  `DayActionSet` that breaks one of the SDK's checks (id formats, duplicate
  ids, line limits, more than five positive RFQs or more than one probe per
  supplier-component pair, a repeated supplier RFQ id) raises
  `ContractError` when it is built; uncaught, `runtime.serve` exits and
  the seat plays default actions for the rest of the game. For the RFQ
  counts this is harsher than the engine, which would only reject the extra
  RFQs. Build actions within these limits, or catch the error in `act` and
  return a smaller action.

## 3. Limits per seat

Every seat runs in its own container. Requests equal limits, so what is
listed is guaranteed and is also the ceiling:

| | |
|---|---|
| CPU | 2 cores (a hard CPU quota of 2 cores' time) on an Intel Xeon Silver 4216 at 2.1 GHz |
| Memory | 8 GiB, no swap; above it the kernel kills the agent |
| Network | none; no sockets at all (section 7) |
| Daily deadline | 15 s per day from the observation (`rules/TIMING.md` section 2) |
| Startup grace | 30 s between the start of the six agents and the day-0 observation |
| GPU | none, unless requested (section 5) |

- `os.cpu_count()` and `nproc` report the whole machine (64), not your
  2 cores. The thread variables above cap numpy's and PyTorch's default
  pools; size any other pool yourself (`torch.set_num_threads(2)`, a
  process pool of at most 2). More threads than cores only make each
  decision slower under the quota.
- A core of the tournament's machines is about 2.5 to 3 times slower than
  a recent laptop's (TacTex-inspired v4: 110 s of CPU per game on an Apple
  M-series laptop, about 290 s there). Leave room for that in your time
  budget.
- The memory limit counts everything in the container, including files
  read into the page cache. An agent killed for memory is out for the rest
  of the game (`process-exit`, default actions until day 219).
- What the limits mean in practice, measured on the kit's house agents and
  the organiser's agent:

  | Agent | Peak memory | CPU per game | Slowest decision |
  |---|---|---|---|
  | v1 house agents (13) | under 50 MiB | about 2 s (laptop) | under 0.1 s |
  | Template agent | about 30 MiB | about 4 s | under 0.1 s |
  | Botticelli-inspired v4 | about 80 MiB | about 11 s | 0.2 s |
  | Botticelli-inspired v4-1 | about 80 MiB | about 220 s | 7.4 s |
  | DeepMaize-inspired v4 m2 | about 0.5 GiB | about 110 s | 0.8 s |
  | TacTex-inspired v4 m3-m5 | about 0.25 GiB | 290 to 420 s | 5.0 s |
  | Organiser's agent | about 4 GiB | 900 to 1,300 s | 10.4 s |

  CPU and slowest decision are from full games on the tournament's
  machines at 2 cores per seat (the v1 CPU from a laptop); peak memory is
  the larger of the resident memory on a laptop and the container's memory
  sampled on the tournament's machines.
- The seat class above is not changed after the invitation is sent.

## 4. Startup

The runner starts the six seats, sends each its `GameBootstrap`, waits the
30 s startup grace, then sends the day-0 observation (`rules/TIMING.md`
section 5). On the tournament's machines `import torch` (2.9, CPU) takes
about 2.5 s in a fresh container and a large in-house model stack about
3 s, so 30 s leaves room for loading models of a few hundred MB. An agent
that is still loading when day 0 opens keeps its input (it is buffered) and
loses only the time; if it misses the day-0 deadline it defaults that day
only. Day 0 has no customer RFQs. The image is pulled and the container
started before the runner starts, outside the grace: an image with
PyTorch (CPU) pulls in about 35 s on the tournament's machines, so
images of a few GB are fine.

## 5. GPU on request

A seat may ask for one NVIDIA GeForce RTX 2080 Ti (11 GB; driver 580, CUDA
up to 13.0) in addition to the CPU and memory above; the image must bring
its own CUDA runtime libraries (for example a PyTorch CUDA wheel). Say so
when you submit. The GPU machines are the same Xeon Silver 4216 machines
as the CPU-only games run on, so a game with a GPU seat gives every seat,
the GPU seat's CPU part included, the CPU class of section 3. A GPU seat
does different work per decision than a CPU seat within the same 15 s, so
the results table marks GPU seats. The organiser's agent plays on CPU.

## 6. Faults and replays

- **Infrastructure faults are replayed**: a node failure, an evicted or
  preempted pod, an image that cannot be pulled, a runner crash, or any
  game that ends without a result for a reason that is not an agent's
  action. The game is played again from the same root with the same seats
  and images, and only the replay counts.
- **An agent's own failures are not replayed**: a slow decision (that day
  defaults), a crash, a memory kill, a malformed message or an oversized
  frame (default actions for the rest of the game), as `rules/TIMING.md`
  section 3 describes. Its scores stand.

## 7. How the tournament runs a seat

Locally, a seat is exactly
`docker run -i --network none --cpus 2 --memory 8g --memory-swap 8g
-e OMP_NUM_THREADS=2 -e OPENBLAS_NUM_THREADS=2 -e MKL_NUM_THREADS=2
--entrypoint /agent/run IMAGE`, launched by the game runner over stdin and
stdout; `runner/play_game.py` does this for every `image:` seat. On the
tournament's cluster each game is one pod: the game runner in one
container, and each seat in its own container from its own image with the
limits above, started with `/agent/run` and connected to the runner by
named pipes in a shared volume, and a network policy that denies
the pod all outgoing traffic. The agent sees the same stdin and stdout,
2 cores, 8 GiB and a fresh filesystem in both, with two differences: the
containers of a pod share one network namespace, so on the cluster the
loopback interface is shared by the six seats and the runner of a game,
where locally each seat has its own; and the cluster's own policies admit
incoming connections to a pod from other workloads of the cluster, so a
socket listening on the pod's address could be reached from outside the
game.

**Rule: an agent must not open any network socket** (no listening, no
connecting, on loopback or anywhere else) and must not try to communicate
with the other seats by any channel but the game. The tournament runner
samples the game pod's open sockets every second and each seat's own
sockets every five seconds and at the end of the game; an agent that
breaks the rule is excluded from the results (`FORMAT.md` section 10).

Without Docker, `runner/play_game.py --process-limits` with your `run` as a
local executable approximates the limits at process level (memory
watchdog, thread caps, network denied where the OS allows); it is a check,
not the tournament's sandbox (`runner/README.md`).
