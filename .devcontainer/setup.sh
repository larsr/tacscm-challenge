#!/usr/bin/env bash
# Builds the server, syncs the Python environment and fetches the DeepMaize database. Safe to rerun.
set -euo pipefail
cd "$(dirname "$0")/.."

(cd server && cargo build --release --locked)
uv sync --locked

db=agents/models/deepmaize-v4-m2/pc-database.npz
sha=d5059386e7fc232700c839bf73be3f212ad266cd0b01559e79b62144935471ca
if ! echo "$sha  $db" | sha256sum --check --status 2>/dev/null; then
    curl -fL --retry 3 -o "$db.part" \
        https://github.com/rekrevs/tacscm-challenge/releases/download/v1.0/pc-database.npz
    echo "$sha  $db.part" | sha256sum --check --quiet
    mv "$db.part" "$db"
fi
uv run python -m tac_scm_gallery.house --check-models
