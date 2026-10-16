#!/usr/bin/env bash
# Manage the dev pod on a Kubernetes cluster. Needs kubectl, envsubst and the settings in .env (see .env.example).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
env_file=${ENV_FILE:-$here/../.env}
[[ -f $env_file ]] || { echo "missing $env_file; copy .env.example and fill it in" >&2; exit 1; }
set -a
# shellcheck source=/dev/null
source "$env_file"
set +a

vars=(KUBE_CONTEXT KUBE_NAMESPACE DEVPOD_NAME DEVPOD_IMAGE DEVPOD_PULL_SECRET DEVPOD_PVC DEVPOD_SUBPATH
      DEVPOD_REPO_URL DEVPOD_CPU DEVPOD_MEMORY)
for var in "${vars[@]}"; do
    [[ -n ${!var:-} ]] || { echo "$var is not set in $env_file" >&2; exit 1; }
done

NAME=$DEVPOD_NAME
k() { kubectl --context "$KUBE_CONTEXT" --namespace "$KUBE_NAMESPACE" "$@"; }
# Only the listed variables are substituted, so the pod's own shell variables survive.
render() { envsubst "$(printf '${%s} ' "${vars[@]}")" < "$here/devpod.template.yaml"; }

case "${1:-}" in
    up)
        render | k apply -f -
        k scale deployment/"$NAME" --replicas=1
        echo "waiting for setup (server build, uv sync, model download)..."
        k rollout status deployment/"$NAME" --timeout=20m
        echo "ready. Attach VS Code: Kubernetes view > pod $NAME-* > 'Attach Visual Studio Code', or: $0 shell"
        ;;
    down)
        k scale deployment/"$NAME" --replicas=0
        ;;
    status)
        k get deployment,pods -l app="$NAME"
        ;;
    shell)
        k exec -it deployment/"$NAME" -- bash -l
        ;;
    logs)
        k logs -f deployment/"$NAME"
        ;;
    render)
        render
        ;;
    delete)
        k delete deployment/"$NAME"
        ;;
    *)
        echo "usage: $0 up|down|status|shell|logs|render|delete" >&2
        echo "  the checkout stays in \$DEVPOD_SUBPATH on volume \$DEVPOD_PVC. Settings: $env_file" >&2
        exit 2
        ;;
esac
