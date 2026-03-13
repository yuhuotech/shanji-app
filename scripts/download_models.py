#!/usr/bin/env python3
"""
闪记（Shanji）模型下载脚本

用于下载预编译的 ONNX 模型文件。
"""

import argparse
import hashlib
import os
import sys
from pathlib import Path
from urllib.parse import urlparse

import requests
from tqdm import tqdm


# 社区维护的 ONNX 模型列表
MODELS = {
    "paraformer-zh": {
        "name": "Paraformer 中文",
        "description": "中文语音识别模型，支持中英混合",
        "url": "https://huggingface.co/FunAudioLLM/SenseVoiceSmall/resolve/main/model.onnx",
        "size_mb": 230,
        "sha256": None,  # 需要实际计算后填入
    },
    "silero-vad": {
        "name": "Silero VAD",
        "description": "语音活动检测模型",
        "url": "https://github.com/snakers4/silero-vad/raw/master/files/silero_vad.onnx",
        "size_mb": 1,
        "sha256": None,
    },
}


def get_data_dir():
    """获取应用数据目录"""
    system = sys.platform

    if system == "darwin":
        return Path.home() / "Library/Application Support/shanji"
    elif system == "win32":
        return Path(os.environ.get("APPDATA", "")) / "shanji"
    else:  # Linux
        return Path.home() / ".local/share/shanji"


def download_file(url: str, dest_path: Path, desc: str = "Downloading"):
    """
    下载文件并显示进度条

    Args:
        url: 文件 URL
        dest_path: 目标路径
        desc: 进度条描述

    Returns:
        是否成功
    """
    try:
        response = requests.get(url, stream=True, timeout=30)
        response.raise_for_status()

        total_size = int(response.headers.get("content-length", 0))
        block_size = 8192

        dest_path.parent.mkdir(parents=True, exist_ok=True)

        with open(dest_path, "wb") as f, tqdm(
            desc=desc,
            total=total_size,
            unit="B",
            unit_scale=True,
            unit_divisor=1024,
        ) as pbar:
            for chunk in response.iter_content(chunk_size=block_size):
                if chunk:
                    f.write(chunk)
                    pbar.update(len(chunk))

        return True

    except Exception as e:
        print(f"✗ 下载失败: {e}")
        if dest_path.exists():
            dest_path.unlink()
        return False


def verify_sha256(file_path: Path, expected_hash: str) -> bool:
    """验证文件 SHA256"""
    sha256_hash = hashlib.sha256()

    with open(file_path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            sha256_hash.update(chunk)

    actual_hash = sha256_hash.hexdigest()
    return actual_hash == expected_hash


def download_model(model_id: str, force: bool = False) -> bool:
    """
    下载指定模型

    Args:
        model_id: 模型 ID
        force: 强制重新下载

    Returns:
        是否成功
    """
    if model_id not in MODELS:
        print(f"✗ 未知模型: {model_id}")
        print(f"可用模型: {', '.join(MODELS.keys())}")
        return False

    model_info = MODELS[model_id]
    data_dir = get_data_dir()
    model_dir = data_dir / "models" / model_id

    print(f"\n模型: {model_info['name']}")
    print(f"描述: {model_info['description']}")
    print(f"大小: ~{model_info['size_mb']} MB")
    print(f"目录: {model_dir}")

    # 检查是否已存在
    if model_dir.exists() and not force:
        print(f"⚠ 模型已存在，使用 --force 重新下载")
        return True

    # 创建目录
    model_dir.mkdir(parents=True, exist_ok=True)

    # 下载文件
    url = model_info["url"]
    filename = Path(urlparse(url).path).name or "model.onnx"
    dest_path = model_dir / filename

    print(f"\n开始下载...")
    if not download_file(url, dest_path, desc=model_info["name"]):
        return False

    # 验证 SHA256
    if model_info.get("sha256"):
        print("验证 SHA256...")
        if not verify_sha256(dest_path, model_info["sha256"]):
            print("✗ SHA256 验证失败，文件可能损坏")
            dest_path.unlink()
            return False
        print("✓ SHA256 验证通过")

    print(f"✓ 下载完成: {dest_path}")
    return True


def list_models():
    """列出可用模型"""
    print("\n可用模型:\n")
    print(f"{'ID':<20} {'名称':<20} {'大小':<10} {'描述'}")
    print("-" * 80)

    for model_id, info in MODELS.items():
        print(f"{model_id:<20} {info['name']:<20} ~{info['size_mb']:<9}MB {info['description']}")


def main():
    parser = argparse.ArgumentParser(
        description="下载闪记（Shanji）ASR 模型",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  # 列出可用模型
  python download_models.py --list

  # 下载模型
  python download_models.py --model paraformer-zh

  # 强制重新下载
  python download_models.py --model paraformer-zh --force
        """
    )

    parser.add_argument(
        "--model",
        type=str,
        help="要下载的模型 ID",
    )

    parser.add_argument(
        "--list",
        action="store_true",
        help="列出可用模型",
    )

    parser.add_argument(
        "--force",
        action="store_true",
        help="强制重新下载",
    )

    parser.add_argument(
        "--output-dir",
        type=str,
        help="自定义输出目录",
    )

    args = parser.parse_args()

    print("=" * 60)
    print("闪记（Shanji）模型下载工具")
    print("=" * 60)

    if args.list:
        list_models()
        return

    if not args.model:
        print("\n✗ 请指定模型 ID 或使用 --list 查看可用模型")
        parser.print_help()
        sys.exit(1)

    success = download_model(args.model, force=args.force)
    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()
