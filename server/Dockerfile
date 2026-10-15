# The challenge kit as one image: the game runner, the SDK and the house
# agents. Build from the repository root, with the release-asset models in
# agents/models/ (agents/MODELS.md):
#   docker build -f server/Dockerfile -t tacscm-kit .

FROM rust:1.96-slim-bookworm AS server
WORKDIR /build
COPY server/Cargo.toml server/Cargo.lock ./
COPY server/crates ./crates
RUN cargo build --release --locked --bin tac-scm-game-runner

FROM python:3.12-slim-bookworm
ENV PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    TAC_SCM_HOUSE_MODELS=/kit/agents/models
WORKDIR /kit
COPY sdk ./sdk
COPY agents ./agents
COPY runner ./runner
RUN pip install -r agents/requirements.txt ./sdk -e ./agents
COPY --from=server /build/target/release/tac-scm-game-runner ./server/target/release/tac-scm-game-runner
CMD ["python", "runner/play_game.py", "--help"]
