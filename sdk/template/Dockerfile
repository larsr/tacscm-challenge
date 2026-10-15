# A submission image: the kit's SDK, your agent directory at /agent, and
# /agent/run as the command that starts it (tournament/SUBMISSION.md).
#
# Build from the root of the kit, for linux/amd64:
#   docker build --platform linux/amd64 -f sdk/template/Dockerfile -t my-agent .
# with your own agent directory (a copy of sdk/template with your code, an
# executable `run`, and optionally a pinned requirements.txt):
#   docker build --platform linux/amd64 -f sdk/template/Dockerfile \
#     --build-arg AGENT=path/to/my-agent -t my-agent .
# A zip submission is built by the organiser with exactly this file.

FROM python:3.12-slim-bookworm
ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

COPY sdk/pyproject.toml sdk/README.md /tmp/sdk/
COPY sdk/tac_scm_sdk /tmp/sdk/tac_scm_sdk
COPY sdk/tac_scm_baselines /tmp/sdk/tac_scm_baselines
RUN pip install /tmp/sdk && rm -rf /tmp/sdk

ARG AGENT=sdk/template
COPY ${AGENT} /agent
RUN if [ -f /agent/requirements.txt ]; then pip install -r /agent/requirements.txt; fi \
 && test -f /agent/run && chmod 755 /agent/run \
 && rm -rf /agent/__pycache__

WORKDIR /agent
ENTRYPOINT ["/agent/run"]
