#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REGISTRY_PATH="${ROOT_DIR}/public/model_registry.json"
OUTPUT_DIR="${ROOT_DIR}/dist-models"
RELEASE_TAG="models"
LANGUAGE="zh"
UPLOAD="true"
REPO=""

usage() {
  cat <<'EOF'
Usage:
  scripts/publish_model_release.sh \
    --model-id paraformer-zh \
    --name "Paraformer 中文" \
    --description "中文普通话，支持中英混合" \
    --version v1.0.0 \
    --model-dir ./models/paraformer-zh

Options:
  --model-id         Registry model id
  --name             Model display name
  --description      Model description
  --version          Asset version, e.g. v1.0.0
  --model-dir        Local ONNX model directory
  --language         Language code, default: zh
  --repo             GitHub repo slug, default: derived from git remote origin
  --release-tag      Release tag used for model assets, default: models
  --registry         Path to model_registry.json
  --output-dir       Directory for packaged tar.gz
  --package-only     Only package and update registry, do not upload

Requirements for upload:
  - gh CLI installed
  - gh auth login already completed
EOF
}

require_arg() {
  local name="$1"
  local value="${2:-}"
  if [[ -z "${value}" ]]; then
    echo "missing required argument: ${name}" >&2
    usage
    exit 1
  fi
}

repo_from_git_remote() {
  local remote_url
  remote_url="$(git -C "${ROOT_DIR}" remote get-url origin)"

  if [[ "${remote_url}" =~ ^git@github\.com:(.+)/(.+)\.git$ ]]; then
    echo "${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
    return
  fi

  if [[ "${remote_url}" =~ ^https://github\.com/(.+)/(.+)\.git$ ]]; then
    echo "${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
    return
  fi

  if [[ "${remote_url}" =~ ^https://github\.com/(.+)/(.+)$ ]]; then
    echo "${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
    return
  fi

  echo "unable to parse GitHub repo from origin: ${remote_url}" >&2
  exit 1
}

ensure_git_repo_ready() {
  if ! git -C "${ROOT_DIR}" rev-parse --verify HEAD >/dev/null 2>&1; then
    cat >&2 <<'EOF'
GitHub Releases cannot be created from an empty repository.

This repository has no commits yet. Do this first:
  1. git add .
  2. git commit -m "Initial commit"
  3. git push -u origin main

If you only want to generate the local tar.gz and registry for now, rerun with:
  --package-only
EOF
    exit 1
  fi
}

ensure_release_exists() {
  local repo="$1"
  local tag="$2"

  if gh release view "${tag}" --repo "${repo}" >/dev/null 2>&1; then
    return
  fi

  gh release create "${tag}" \
    --repo "${repo}" \
    --title "Model Assets" \
    --notes "Release channel for Shanji model packages and model_registry.json"
}

MODEL_ID=""
NAME=""
DESCRIPTION=""
VERSION=""
MODEL_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model-id)
      MODEL_ID="$2"
      shift 2
      ;;
    --name)
      NAME="$2"
      shift 2
      ;;
    --description)
      DESCRIPTION="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --model-dir)
      MODEL_DIR="$2"
      shift 2
      ;;
    --language)
      LANGUAGE="$2"
      shift 2
      ;;
    --repo)
      REPO="$2"
      shift 2
      ;;
    --release-tag)
      RELEASE_TAG="$2"
      shift 2
      ;;
    --registry)
      REGISTRY_PATH="$2"
      shift 2
      ;;
    --output-dir)
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --package-only)
      UPLOAD="false"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

require_arg "--model-id" "${MODEL_ID}"
require_arg "--name" "${NAME}"
require_arg "--description" "${DESCRIPTION}"
require_arg "--version" "${VERSION}"
require_arg "--model-dir" "${MODEL_DIR}"

if [[ -z "${REPO}" ]]; then
  REPO="$(repo_from_git_remote)"
fi

DOWNLOAD_BASE_URL="https://github.com/${REPO}/releases/download/${RELEASE_TAG}"
ASSET_PATH="${OUTPUT_DIR}/${MODEL_ID}-${VERSION}.tar.gz"

echo "repo: ${REPO}"
echo "release tag: ${RELEASE_TAG}"
echo "model id: ${MODEL_ID}"
echo "model dir: ${MODEL_DIR}"

python3 "${ROOT_DIR}/scripts/package_model.py" \
  --model-id "${MODEL_ID}" \
  --name "${NAME}" \
  --language "${LANGUAGE}" \
  --description "${DESCRIPTION}" \
  --version "${VERSION}" \
  --model-dir "${MODEL_DIR}" \
  --registry "${REGISTRY_PATH}" \
  --output-dir "${OUTPUT_DIR}" \
  --download-base-url "${DOWNLOAD_BASE_URL}"

echo
echo "packaged asset: ${ASSET_PATH}"
echo "updated registry: ${REGISTRY_PATH}"

if [[ "${UPLOAD}" != "true" ]]; then
  echo
  echo "package-only mode enabled, skipping GitHub upload"
  exit 0
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "gh CLI is required for upload. Install it or rerun with --package-only." >&2
  exit 1
fi

ensure_git_repo_ready
ensure_release_exists "${REPO}" "${RELEASE_TAG}"

gh release upload "${RELEASE_TAG}" \
  "${ASSET_PATH}" \
  "${REGISTRY_PATH}" \
  --repo "${REPO}" \
  --clobber

echo
echo "uploaded to: https://github.com/${REPO}/releases/tag/${RELEASE_TAG}"
echo "app download registry: ${DOWNLOAD_BASE_URL}/model_registry.json"
