#!/usr/bin/env python3
"""
Register a local model directory for direct-file GitHub release downloads.
"""

from __future__ import annotations

import argparse
import json
from datetime import date
from pathlib import Path


DEFAULT_REGISTRY_PATH = Path(__file__).resolve().parents[1] / "public" / "model_registry.json"

WHOLE_SUFFIX_TO_ROLE = {
    "-model.onnx": "model",
    "-model-quant.onnx": "modelQuant",
    "-config.yaml": "config",
    "-am.mvn": "meanVariance",
    "-vocab.txt": "vocab",
}

STREAMING_SUFFIX_TO_ROLE = {
    "-encoder.onnx": "encoder",
    "-encoder-quant.onnx": "encoderQuant",
    "-decoder.onnx": "decoder",
    "-decoder-quant.onnx": "decoderQuant",
    "-config.yaml": "config",
    "-am.mvn": "meanVariance",
    "-vocab.txt": "vocab",
}


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


def collect_artifacts(model_id: str, backend: str, model_dir: Path) -> list[dict]:
    suffix_map = WHOLE_SUFFIX_TO_ROLE if backend == "whole" else STREAMING_SUFFIX_TO_ROLE
    prefix = f"{model_id}-"
    artifacts = []

    for path in sorted(model_dir.iterdir()):
        if not path.is_file():
            continue
        file_name = path.name
        if not file_name.startswith(prefix):
            raise SystemExit(
                f"unexpected file name in {model_dir}: {file_name} does not start with {prefix}"
            )

        role = next(
            (mapped_role for suffix, mapped_role in suffix_map.items() if file_name.endswith(suffix)),
            None,
        )
        if role is None:
            raise SystemExit(f"cannot infer artifact role for {file_name}")

        artifacts.append(
            {
                "role": role,
                "fileName": file_name,
                "sizeBytes": path.stat().st_size,
                "sha256": "",
            }
        )

    return artifacts


def validate_artifacts(model_id: str, backend: str, artifacts: list[dict]) -> None:
    roles = {artifact["role"] for artifact in artifacts}
    if backend == "whole":
        required = {"config", "meanVariance", "vocab"}
        if "model" not in roles and "modelQuant" not in roles:
            raise SystemExit(f"model {model_id} must contain at least one of model/modelQuant")
    else:
        required = {"encoder", "decoder", "config", "meanVariance", "vocab"}

    missing = sorted(required - roles)
    if missing:
        raise SystemExit(f"model {model_id} is missing required artifact roles: {', '.join(missing)}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Register a Shanji model directory for direct-file downloads")
    parser.add_argument("--model-id", required=True, help="registry model id, e.g. paraformer-zh-streaming")
    parser.add_argument("--name", required=True, help="display name")
    parser.add_argument("--language", default="zh", help="language code")
    parser.add_argument("--description", required=True, help="model description")
    parser.add_argument("--backend", required=True, choices=["whole", "streaming"], help="runtime backend type")
    parser.add_argument("--version", required=True, help="version, e.g. v1.0.0")
    parser.add_argument("--model-dir", required=True, help="directory containing standardized model files")
    parser.add_argument("--registry", default=str(DEFAULT_REGISTRY_PATH), help="path to model_registry.json")
    parser.add_argument(
        "--download-base-url",
        required=True,
        help="base URL used to download individual files, e.g. https://github.com/org/repo/releases/download/models",
    )
    parser.add_argument("--sha256", default="", help="optional overall sha256 placeholder")
    args = parser.parse_args()

    model_dir = Path(args.model_dir).resolve()
    registry_path = Path(args.registry).resolve()

    artifacts = collect_artifacts(args.model_id, args.backend, model_dir)
    validate_artifacts(args.model_id, args.backend, artifacts)
    registry = load_registry(registry_path)

    entry = {
        "id": args.model_id,
        "name": args.name,
        "language": args.language,
        "description": args.description,
        "backend": args.backend,
        "sizeBytes": sum(artifact["sizeBytes"] for artifact in artifacts),
        "downloadUrl": args.download_base_url.rstrip("/"),
        "sha256": args.sha256,
        "version": args.version,
        "artifacts": artifacts,
    }

    upsert_model_entry(registry, entry)
    save_registry(registry_path, registry)

    print(f"registry: {registry_path}")
    print(f"download base url: {entry['downloadUrl']}")
    print("artifacts:")
    for artifact in artifacts:
        print(f"  - {artifact['role']}: {artifact['fileName']}")


if __name__ == "__main__":
    main()
