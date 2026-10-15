#!/usr/bin/env bash
# Manage the TAC SCM dev pod on ICE (ice/devpod.yaml). Needs kubectl and a kubeconfig context for ICE.
set -euo pipefail

CONTEXT=${KUBE_CONTEXT:-icekube}
NAMESPACE=${KUBE_NAMESPACE:-aic}
NAME=tacscm-dev
k() { kubectl --context "$CONTEXT" --namespace "$NAMESPACE" "$@"; }

case "${1:-}" in
    up)
        k apply -f "$(dirname "$0")/devpod.yaml"
        k scale deployment/$NAME --replicas=1
        echo "waiting for setup (server build, uv sync, model download)..."
        k rollout status deployment/$NAME --timeout=20m
        echo "ready. Attach VS Code: Kubernetes view > pod $NAME-* > 'Attach Visual Studio Code', or: $0 shell"
        ;;
    down)
        k scale deployment/$NAME --replicas=0
        ;;
    status)
        k get deployment,pods -l app=$NAME
        ;;
    shell)
        k exec -it deployment/$NAME -- bash -l
        ;;
    logs)
        k logs -f deployment/$NAME
        ;;
    delete)
        k delete deployment/$NAME
        ;;
    *)
        echo "usage: $0 up|down|status|shell|logs|delete" >&2
        echo "  the checkout stays in tacscm-challenge/ on larsr-disk. KUBE_CONTEXT/KUBE_NAMESPACE override icekube/aic." >&2
        exit 2
        ;;
esac
