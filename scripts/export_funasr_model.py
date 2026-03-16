#!/usr/bin/env python3
"""
FunASR ONNX 模型导出脚本

用于从 FunASR 导出 Paraformer 模型为 ONNX 格式，供闪记（Shanji）使用。
当前默认使用 FunASR 官方导出，生成 `model.onnx` / `vocab.txt` / `config.yaml` / `am.mvn`。
仅在需要兼容旧链路时才使用 `--manual`。

依赖:
    python3 -m pip install funasr modelscope torch torchaudio onnx onnxscript

用法:
    python export_funasr_model.py --model paraformer --output ./models/paraformer-zh

支持的模型:
    - paraformer (中文非流式，官方导出别名)
    - paraformer-zh-streaming (中文流式)
"""

import argparse
import json
import os
import sys
from pathlib import Path


def resolve_export_model_id(model_id: str) -> str:
    model_aliases = {
        "iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-pytorch": "paraformer",
        "iic/speech_paraformer-large_asr_nat-zh-cn-16k-common-vocab8404-online": "paraformer-zh-streaming",
    }
    return model_aliases.get(model_id, model_id)


def should_force_manual_export(model_id: str) -> bool:
    resolved = resolve_export_model_id(model_id)
    return resolved == "paraformer-zh-streaming"


def copy_support_files(model, output_path: Path):
    model_root = getattr(model, "model_path", None) or getattr(model, "model_dir", None)
    if not model_root:
        return

    model_root = Path(model_root)
    vocab_dst = output_path / "vocab.txt"
    vocab_txt_src = model_root / "tokens.txt"
    vocab_json_src = model_root / "tokens.json"

    if vocab_txt_src.exists():
        import shutil
        shutil.copy2(vocab_txt_src, vocab_dst)
        print(f"✓ 词汇表复制完成: {vocab_dst}")
    elif vocab_json_src.exists():
        tokens = json.loads(vocab_json_src.read_text(encoding="utf-8"))
        if isinstance(tokens, dict):
            ordered_tokens = [
                token for token, _ in sorted(tokens.items(), key=lambda item: item[1])
            ]
        elif isinstance(tokens, list):
            ordered_tokens = tokens
        else:
            raise RuntimeError(
                f"Unsupported tokens.json format: {type(tokens).__name__}"
            )

        with open(vocab_dst, "w", encoding="utf-8") as handle:
            for token in ordered_tokens:
                handle.write(f"{token}\n")
        print(f"✓ 词汇表生成完成: {vocab_dst}")
    else:
        print(f"⚠ 未找到词汇表文件: {vocab_txt_src} / {vocab_json_src}")

    config_src = model_root / "config.yaml"
    config_dst = output_path / "config.yaml"
    if config_src.exists():
        import shutil
        shutil.copy2(config_src, config_dst)
        print(f"✓ 配置文件复制完成: {config_dst}")

    mean_variance_src = model_root / "am.mvn"
    mean_variance_dst = output_path / "am.mvn"
    if mean_variance_src.exists():
        import shutil
        shutil.copy2(mean_variance_src, mean_variance_dst)
        print(f"✓ 均值方差文件复制完成: {mean_variance_dst}")


def check_dependencies():
    """检查必要的依赖是否已安装"""
    try:
        import funasr
        import modelscope
        import torch
        import torchaudio
        import onnx
        import onnxscript
        print(f"✓ funasr {funasr.__version__}")
        print(f"✓ modelscope {modelscope.__version__}")
        print(f"✓ torch {torch.__version__}")
        print(f"✓ torchaudio {torchaudio.__version__}")
        print(f"✓ onnx {onnx.__version__}")
        print(f"✓ onnxscript {onnxscript.__version__}")
        return True
    except ImportError as e:
        print(f"✗ 缺少依赖: {e}")
        print("\n请安装依赖:")
        print("  python3 -m pip install funasr modelscope torch torchaudio onnx onnxscript")
        return False

def export_model(model_id: str, output_dir: str, device: str = "cpu"):
    """
    从 FunASR 导出 ONNX 模型

    Args:
        model_id: ModelScope 模型 ID
        output_dir: 输出目录
        device: 导出设备 (cpu/cuda)
    """
    from funasr import AutoModel
    import torch.onnx

    output_path = Path(output_dir)
    output_path.mkdir(parents=True, exist_ok=True)
    model_name = resolve_export_model_id(model_id)

    print(f"\n正在加载模型: {model_name}")
    print(f"输出目录: {output_path.absolute()}")

    # 加载模型
    model = AutoModel(
        model=model_name,
        device=device,
        disable_update=True,
    )

    print(f"模型加载完成，开始导出 ONNX...")

    try:
        original_export = torch.onnx.export

        def legacy_export(*args, **kwargs):
            kwargs.setdefault("dynamo", False)
            return original_export(*args, **kwargs)

        torch.onnx.export = legacy_export
        try:
            export_dir = model.export(
                type="onnx",
                quantize=False,  # 暂不量化，保持精度
                opset_version=14,
                output_dir=str(output_path),
            )
        finally:
            torch.onnx.export = original_export
        print(f"✓ ONNX 模型导出完成: {export_dir}")
        copy_support_files(model, output_path)

        print(f"\n导出完成！模型文件位于: {output_path.absolute()}")
        print(f"\n文件结构:")
        for f in output_path.rglob("*"):
            if f.is_file():
                rel = f.relative_to(output_path)
                size = f.stat().st_size / (1024 * 1024)  # MB
                print(f"  {rel} ({size:.1f} MB)")

        return True

    except Exception as e:
        print(f"✗ 导出失败: {e}")
        import traceback
        traceback.print_exc()
        return False

def export_model_manual(model_id: str, output_dir: str, device: str = "cpu"):
    """
    手动导出 ONNX 模型（如果 FunASR 内置导出不可用）

    这会导出一个简化的 encoder/decoder 结构。
    """
    import torch
    import torch.onnx
    from funasr import AutoModel

    output_path = Path(output_dir)
    output_path.mkdir(parents=True, exist_ok=True)
    model_name = resolve_export_model_id(model_id)

    print(f"\n正在手动导出: {model_name}")

    # 加载模型
    model = AutoModel(
        model=model_name,
        device=device,
        disable_update=True,
    )

    model.model.eval()

    # 导出 Encoder
    print("导出 Encoder...")

    # 准备 dummy input
    batch_size = 1
    seq_len = 100  # 帧数
    feature_dim = 80  # FBank 维度

    dummy_speech = torch.randn(batch_size, seq_len, feature_dim, device=device)
    dummy_speech_lengths = torch.tensor([seq_len], dtype=torch.long, device=device)

    encoder_path = output_path / "encoder.onnx"

    encoder_exported = False
    try:
        torch.onnx.export(
            model.model.encoder,
            (dummy_speech, dummy_speech_lengths),
            str(encoder_path),
            input_names=["speech", "speech_lengths"],
            output_names=["encoder_out", "encoder_out_lens"],
            dynamic_axes={
                "speech": {0: "batch_size", 1: "seq_len"},
                "speech_lengths": {0: "batch_size"},
                "encoder_out": {0: "batch_size", 1: "seq_len"},
                "encoder_out_lens": {0: "batch_size"},
            },
            opset_version=14,
            do_constant_folding=True,
            dynamo=False,
        )
        print(f"✓ Encoder 导出完成: {encoder_path}")
        encoder_exported = True
    except Exception as e:
        print(f"✗ Encoder 导出失败: {e}")

    # 导出 Decoder (CTC)
    print("导出 Decoder...")

    hidden_dim = model.model.encoder.output_size()

    dummy_encoder_out = torch.randn(batch_size, seq_len, hidden_dim, device=device)

    decoder_path = output_path / "decoder.onnx"

    try:
        # CTC 解码器通常是一个简单的线性层
        torch.onnx.export(
            model.model.ctc.ctc_lo,  # CTC 线性层
            dummy_encoder_out,
            str(decoder_path),
            input_names=["encoder_out"],
            output_names=["ctc_logits"],
            dynamic_axes={
                "encoder_out": {0: "batch_size", 1: "seq_len"},
                "ctc_logits": {0: "batch_size", 1: "seq_len"},
            },
            opset_version=14,
            do_constant_folding=True,
            dynamo=False,
        )
        print(f"✓ Decoder 导出完成: {decoder_path}")
    except Exception as e:
        print(f"✗ Decoder 导出失败: {e}")

    copy_support_files(model, output_path)

    return encoder_exported

def main():
    parser = argparse.ArgumentParser(
        description="导出 FunASR Paraformer 模型为 ONNX 格式",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  # 导出非流式中文模型
  python export_funasr_model.py --model paraformer --output ./models/paraformer-zh

  # 导出流式中文模型
  python export_funasr_model.py --model paraformer-zh-streaming --output ./models/paraformer-zh-streaming

  # 使用 GPU 导出
  python export_funasr_model.py --model ... --output ... --device cuda
        """
    )

    parser.add_argument(
        "--model",
        type=str,
        default="paraformer",
        help="FunASR 模型 ID 或兼容的 ModelScope ID",
    )

    parser.add_argument(
        "--output",
        type=str,
        required=True,
        help="输出目录",
    )

    parser.add_argument(
        "--device",
        type=str,
        default="cpu",
        choices=["cpu", "cuda"],
        help="导出设备 (默认: cpu)",
    )

    parser.add_argument(
        "--manual",
        action="store_true",
        help="使用手动导出模式（如果内置导出失败）",
    )

    parser.add_argument(
        "--check-only",
        action="store_true",
        help="仅检查依赖，不导出",
    )

    args = parser.parse_args()

    print("=" * 60)
    print("FunASR ONNX 模型导出工具")
    print("=" * 60)

    # 检查依赖
    if not check_dependencies():
        sys.exit(1)

    if args.check_only:
        print("\n依赖检查通过！")
        sys.exit(0)

    # 导出模型
    if args.manual or should_force_manual_export(args.model):
        if not args.manual and should_force_manual_export(args.model):
            print("\n检测到流式模型，自动启用 manual 导出模式以兼容当前 Rust 推理链路")
        success = export_model_manual(args.model, args.output, args.device)
    else:
        success = export_model(args.model, args.output, args.device)

    sys.exit(0 if success else 1)

if __name__ == "__main__":
    main()
