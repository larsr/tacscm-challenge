FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

ARG RUST_VERSION=1.99.0
ARG PYTHON_VERSION=3.12
ARG NIX_VERSION=2.34.1

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    UV_PYTHON_INSTALL_DIR=/usr/local/share/uv/python \
    UV_PYTHON_PREFERENCE=only-managed \
    UV_LINK_MODE=copy \
    NIX_SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    PATH=/home/vscode/.nix-profile/bin:/usr/local/cargo/bin:$PATH

COPY --from=ghcr.io/astral-sh/uv:0.8.22 /uv /uvx /usr/local/bin/

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION" -c clippy,rustfmt \
    && chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME" \
    && uv python install "$PYTHON_VERSION"

# Single-user Nix owned by vscode; containers cannot always create the build sandbox's namespaces.
RUN install -d -o vscode -g vscode /nix \
    && mkdir -p /etc/nix && echo "sandbox = false" > /etc/nix/nix.conf
USER vscode
RUN curl -fsSL "https://releases.nixos.org/nix/nix-$NIX_VERSION/install" | sh -s -- --no-daemon --no-modify-profile

# The Haskell tools of haskell-agent/build.nix, from its cabal file and grammar: GHC with the
# agents' dependencies, cabal, haskell-language-server, hlint, fourmolu, BNFC, alex and happy.
COPY --chown=vscode haskell-agent/build.nix haskell-agent/tacscm-hs-agent.cabal /tmp/haskell-agent/
COPY --chown=vscode haskell-agent/grammar /tmp/haskell-agent/grammar
RUN nix-env --install --file /tmp/haskell-agent/build.nix --attr tools && rm -r /tmp/haskell-agent
