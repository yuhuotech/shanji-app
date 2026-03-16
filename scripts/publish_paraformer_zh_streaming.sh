#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REGISTRY_PATH="${ROOT_DIR}/public/model_registry.json"
MODEL_ID="paraformer-zh-streaming"
MODEL_NAME="Paraformer 流式中文"
MODEL_LANGUAGE="zh"
MODEL_DESCRIPTION="中文流式语音识别模型，适合低延迟实时转写"
MODEL_SOURCE="iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-online"
MODEL_DIR="${ROOT_DIR}/models/${MODEL_ID}"
VERSION="v1.0.0"
DEVICE="cpu"
RELEASE_TAG="models"
REPO="yuhuotech/paraformer-zh"
SKIP_EXPORT="false"
MANUAL_EXPORT="true"
REGISTRY_ONLY="false"
MODELSCOPE_CACHE_DIR="${ROOT_DIR}/.modelscope_cache"

usage() {
  cat <<'EOF'
导出 Paraformer 流式中文模型并将所有文件直接上传到 GitHub Release

默认会执行：
1. 如本地模型目录不存在，则从官方 FunASR / ModelScope 导出 ONNX
2. 更新 public/model_registry.json 为“逐文件下载”模式
3. 将导出结果重命名为统一的标准文件名
4. 上传统一命名后的所有模型文件
5. 上传 model_registry.json 到目标仓库 release

用法：
  ./scripts/publish_paraformer_zh_streaming.sh

常用选项：
  --version v1.0.0                版本号
  --device cpu                    导出设备，cpu 或 cuda
  --repo yuhuotech/paraformer-zh  目标 GitHub 仓库
  --release-tag models            Release tag
  --model-dir PATH                自定义模型目录
  --skip-export                   跳过导出，直接上传当前模型目录
  --manual-export                 显式启用手动导出模式（默认已启用）
  --registry-only                 只更新并上传 model_registry.json，不上传模型文件
  --help                          显示帮助

前提：
  1. Python 依赖已安装：funasr / modelscope / torch / torchaudio / onnx / onnxscript
  2. gh 已安装，并执行过 gh auth login
EOF
}

check_python_export_deps() {
  if KMP_DUPLICATE_LIB_OK="${KMP_DUPLICATE_LIB_OK:-TRUE}" python3 - <<'PY'
import importlib.util
import sys

missing = [
    name for name in ("funasr", "modelscope", "torch", "torchaudio", "onnx", "onnxscript")
    if importlib.util.find_spec(name) is None
]

if missing:
    print("missing python modules: " + ", ".join(missing))
    sys.exit(1)
PY
  then
    return
  fi

  cat <<'EOF'

缺少导出模型所需的 Python 依赖。
请先执行：

  python3 -m pip install funasr modelscope torch torchaudio onnx onnxscript
EOF
  exit 1
}

rename_if_exists() {
  local from="$1"
  local to="$2"
  if [[ -f "${from}" ]]; then
    mv "${from}" "${to}"
  fi
}

normalize_model_dir() {
  local model_dir="$1"
  rename_if_exists "${model_dir}/model.onnx" "${model_dir}/${MODEL_ID}-encoder.onnx"
  rename_if_exists "${model_dir}/encoder.onnx" "${model_dir}/${MODEL_ID}-encoder.onnx"
  rename_if_exists "${model_dir}/model_quant.onnx" "${model_dir}/${MODEL_ID}-encoder-quant.onnx"
  rename_if_exists "${model_dir}/encoder_quant.onnx" "${model_dir}/${MODEL_ID}-encoder-quant.onnx"
  rename_if_exists "${model_dir}/encoder.int8.onnx" "${model_dir}/${MODEL_ID}-encoder-quant.onnx"
  rename_if_exists "${model_dir}/decoder.onnx" "${model_dir}/${MODEL_ID}-decoder.onnx"
  rename_if_exists "${model_dir}/decoder_quant.onnx" "${model_dir}/${MODEL_ID}-decoder-quant.onnx"
  rename_if_exists "${model_dir}/decoder.int8.onnx" "${model_dir}/${MODEL_ID}-decoder-quant.onnx"
  rename_if_exists "${model_dir}/config.yaml" "${model_dir}/${MODEL_ID}-config.yaml"
  rename_if_exists "${model_dir}/am.mvn" "${model_dir}/${MODEL_ID}-am.mvn"
  rename_if_exists "${model_dir}/vocab.txt" "${model_dir}/${MODEL_ID}-vocab.txt"
}

ensure_release_exists() {
  if gh release view "${RELEASE_TAG}" --repo "${REPO}" >/dev/null 2>&1; then
    return
  fi

  gh release create "${RELEASE_TAG}" \
    --repo "${REPO}" \
    --title "Model Assets" \
    --notes "Direct-file release channel for Paraformer ONNX models"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="$2"
      shift 2
      ;;
    --device)
      DEVICE="$2"
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
    --model-dir)
      MODEL_DIR="$2"
      shift 2
      ;;
    --skip-export)
      SKIP_EXPORT="true"
      shift
      ;;
    --manual-export)
      MANUAL_EXPORT="true"
      shift
      ;;
    --registry-only)
      REGISTRY_ONLY="true"
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

DOWNLOAD_BASE_URL="https://github.com/${REPO}/releases/download/${RELEASE_TAG}"

echo "repo: ${REPO}"
echo "release tag: ${RELEASE_TAG}"
echo "model id: ${MODEL_ID}"
echo "model source: ${MODEL_SOURCE}"
echo "model dir: ${MODEL_DIR}"
echo "download base url: ${DOWNLOAD_BASE_URL}"

if [[ "${SKIP_EXPORT}" != "true" ]]; then
  HAS_MODEL_EXPORT="false"
  if [[ -f "${MODEL_DIR}/${MODEL_ID}-encoder.onnx" ]]; then
    HAS_MODEL_EXPORT="true"
  fi

  if [[ "${HAS_MODEL_EXPORT}" == "true" && -f "${MODEL_DIR}/${MODEL_ID}-vocab.txt" ]]; then
    echo
    echo "detected existing exported model, skipping export"
  else
    echo
    echo "exporting official streaming model to ONNX..."
    echo "using OpenMP compatibility mode for local export"
    echo "using local ModelScope cache: ${MODELSCOPE_CACHE_DIR}"
    check_python_export_deps
    EXPORT_ARGS=(
      "python3"
      "${ROOT_DIR}/scripts/export_funasr_model.py"
      "--model" "${MODEL_SOURCE}"
      "--output" "${MODEL_DIR}"
      "--device" "${DEVICE}"
    )

    if [[ "${MANUAL_EXPORT}" == "true" ]]; then
      EXPORT_ARGS+=("--manual")
    fi

    KMP_DUPLICATE_LIB_OK="${KMP_DUPLICATE_LIB_OK:-TRUE}" \
    OMP_NUM_THREADS="${OMP_NUM_THREADS:-1}" \
    MODELSCOPE_CACHE="${MODELSCOPE_CACHE:-${MODELSCOPE_CACHE_DIR}}" \
    "${EXPORT_ARGS[@]}"
  fi
fi

normalize_model_dir "${MODEL_DIR}"

echo
echo "updating registry for direct-file downloads..."
python3 "${ROOT_DIR}/scripts/register_model_dir.py" \
  --model-id "${MODEL_ID}" \
  --name "${MODEL_NAME}" \
  --language "${MODEL_LANGUAGE}" \
  --description "${MODEL_DESCRIPTION}" \
  --backend "streaming" \
  --version "${VERSION}" \
  --model-dir "${MODEL_DIR}" \
  --registry "${REGISTRY_PATH}" \
  --download-base-url "${DOWNLOAD_BASE_URL}"

if ! command -v gh >/dev/null 2>&1; then
  echo "gh CLI is required for upload." >&2
  exit 1
fi

ensure_release_exists

echo
echo "uploading registry..."
gh release upload "${RELEASE_TAG}" \
  "${REGISTRY_PATH}" \
  --repo "${REPO}" \
  --clobber

if [[ "${REGISTRY_ONLY}" == "true" ]]; then
  echo
  echo "registry-only mode enabled, skipping model file uploads"
  exit 0
fi

MODEL_FILES=()
while IFS= read -r file; do
  MODEL_FILES+=("${file}")
done < <(find "${MODEL_DIR}" -maxdepth 1 -type f | sort)

if [[ "${#MODEL_FILES[@]}" -eq 0 ]]; then
  echo "no files found in ${MODEL_DIR}" >&2
  exit 1
fi

echo
echo "uploading model files..."
gh release upload "${RELEASE_TAG}" \
  "${MODEL_FILES[@]}" \
  --repo "${REPO}" \
  --clobber

echo
echo "uploaded to: https://github.com/${REPO}/releases/tag/${RELEASE_TAG}"
echo "direct download base: ${DOWNLOAD_BASE_URL}"
