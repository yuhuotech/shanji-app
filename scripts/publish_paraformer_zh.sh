#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODEL_ID="paraformer-zh"
MODEL_NAME="Paraformer 中文"
MODEL_LANGUAGE="zh"
MODEL_DESCRIPTION="中文普通话，支持中英混合"
MODEL_SOURCE="paraformer"
MODEL_DIR="${ROOT_DIR}/models/${MODEL_ID}"
VERSION="v1.0.0"
DEVICE="cpu"
PACKAGE_ONLY="false"
SKIP_EXPORT="false"
MANUAL_EXPORT="false"
MODELSCOPE_CACHE_DIR="${ROOT_DIR}/.modelscope_cache"

usage() {
  cat <<'EOF'
一键导出并发布 Paraformer 中文模型

默认会执行：
1. 如本地模型目录不存在，则从官方 FunASR 模型导出 ONNX
2. 打包为 .tar.gz
3. 更新 public/model_registry.json
4. 上传到 GitHub release tag: models

用法：
  ./scripts/publish_paraformer_zh.sh

常用选项：
  --version v1.0.0       资产版本号
  --device cpu           导出设备，cpu 或 cuda
  --model-dir PATH       自定义本地模型目录
  --skip-export          跳过导出，直接打包当前模型目录
  --manual-export        显式启用手动导出模式（默认关闭）
  --package-only         只打包并更新 registry，不上传 GitHub
  --help                 显示帮助

前提：
  1. Python 依赖已安装：funasr / modelscope / torch / torchaudio / onnx / onnxscript
  2. 若需要上传：已安装 gh，并执行过 gh auth login
EOF
}

check_python_export_deps() {
  if python3 - <<'PY'
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

如果你使用虚拟环境，请先激活虚拟环境再安装。
EOF
  exit 1
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
    --package-only)
      PACKAGE_ONLY="true"
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

echo "model id: ${MODEL_ID}"
echo "model source: ${MODEL_SOURCE}"
echo "model dir: ${MODEL_DIR}"
echo "version: ${VERSION}"

if [[ "${SKIP_EXPORT}" != "true" ]]; then
  HAS_MODEL_EXPORT="false"
  if [[ -f "${MODEL_DIR}/model.onnx" || -f "${MODEL_DIR}/encoder.onnx" ]]; then
    HAS_MODEL_EXPORT="true"
  fi

  if [[ "${HAS_MODEL_EXPORT}" == "true" && -f "${MODEL_DIR}/vocab.txt" ]]; then
    echo
    echo "detected existing exported model, skipping export"
  else
    echo
    echo "exporting official model to ONNX..."
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

echo
echo "packaging and publishing model asset..."
PUBLISH_ARGS=(
  "${ROOT_DIR}/scripts/publish_model_release.sh"
  "--model-id" "${MODEL_ID}"
  "--name" "${MODEL_NAME}"
  "--language" "${MODEL_LANGUAGE}"
  "--description" "${MODEL_DESCRIPTION}"
  "--version" "${VERSION}"
  "--model-dir" "${MODEL_DIR}"
)

if [[ "${PACKAGE_ONLY}" == "true" ]]; then
  PUBLISH_ARGS+=("--package-only")
fi

"${PUBLISH_ARGS[@]}"

echo
echo "done"
