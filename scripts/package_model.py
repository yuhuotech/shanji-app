#!/usr/bin/env python3
"""
Package a local ONNX model directory into a Shanji release asset and update model_registry.json.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import tarfile
from datetime import date
from pathlib import Path


DEFAULT_REGISTRY_PATH = Path(__file__).resolve().parents[1] / "public" / "model_registry.json"
DEFAULT_OUTPUT_DIR = Path(__file__).resolve().parents[1] / "dist-models"
DEFAULT_DOWNLOAD_BASE_URL = "https://github.com/shanji-ai/shanji/releases/latest/download"


def sha256sum(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def collect_files(model_dir: Path) -> list[str]:
    return sorted(
        str(path.relative_to(model_dir))
        for path in model_dir.rglob("*")
        if path.is_file()
    )


def validate_model_dir(model_dir: Path, files: list[str]) -> None:
    if "model.onnx" not in files and "encoder.onnx" not in files:
        raise SystemExit(f"missing required file in {model_dir}: model.onnx or encoder.onnx")

    if "vocab.txt" not in files and "vocab.json" not in files:
        raise SystemExit(f"missing required file in {model_dir}: vocab.txt or vocab.json")


def package_model(model_dir: Path, output_path: Path) -> list[str]:
    files = collect_files(model_dir)
    validate_model_dir(model_dir, files)
    output_path.parent.mkdir(parents=True, exist_ok=True)

    with tarfile.open(output_path, "w:gz") as archive:
        for relative in files:
            archive.add(model_dir / relative, arcname=relative)

    return files


def load_registry(path: Path) -> dict:
    if not path.exists():
        return {
            "version": 1,
            "updated_at": date.today().isoformat(),
            "models": [],
        }

    return json.loads(path.read_text(encoding="utf-8"))


def save_registry(path: Path, registry: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(registry, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )


def upsert_model_entry(registry: dict, entry: dict) -> None:
    models = registry.setdefault("models", [])
    for index, current in enumerate(models):
        if current.get("id") == entry["id"]:
            models[index] = entry
            break
    else:
        models.append(entry)

    registry["updated_at"] = date.today().isoformat()


def main() -> None:
    parser = argparse.ArgumentParser(description="Package a Shanji model release asset")
    parser.add_argument("--model-id", required=True, help="registry model id, e.g. paraformer-zh")
    parser.add_argument("--name", required=True, help="display name")
    parser.add_argument("--language", default="zh", help="language code")
    parser.add_argument("--description", required=True, help="model description")
    parser.add_argument("--version", required=True, help="asset version, e.g. v1.0.0")
    parser.add_argument("--model-dir", required=True, help="directory containing ONNX model files")
    parser.add_argument("--registry", default=str(DEFAULT_REGISTRY_PATH), help="path to model_registry.json")
    parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR), help="directory to place packaged assets")
    parser.add_argument(
        "--download-base-url",
        default=DEFAULT_DOWNLOAD_BASE_URL,
        help="base URL used in registry download_url entries",
    )
    args = parser.parse_args()

    model_dir = Path(args.model_dir).resolve()
    registry_path = Path(args.registry).resolve()
    output_dir = Path(args.output_dir).resolve()
    asset_name = f"{args.model_id}-{args.version}.tar.gz"
    asset_path = output_dir / asset_name

    files = package_model(model_dir, asset_path)
    registry = load_registry(registry_path)

    entry = {
        "id": args.model_id,
        "name": args.name,
        "language": args.language,
        "description": args.description,
        "sizeBytes": asset_path.stat().st_size,
        "downloadUrl": f"{args.download_base_url.rstrip('/')}/{asset_name}",
        "sha256": sha256sum(asset_path),
        "version": args.version,
        "files": files,
    }

    upsert_model_entry(registry, entry)
    save_registry(registry_path, registry)

    print(f"asset: {asset_path}")
    print(f"sha256: {entry['sha256']}")
    print(f"registry: {registry_path}")


if __name__ == "__main__":
    main()
