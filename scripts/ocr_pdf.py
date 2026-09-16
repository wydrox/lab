#!/usr/bin/env python3
"""Lokalny adapter PDF → GLM-OCR. Na stdout wypisuje tylko JSON."""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

VERSION = "glm-ocr-v1"
MAX_PDF_BYTES = 40_000_000
POPPLER_DIRS = ("/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin")


def poppler_bin(name):
    for directory in POPPLER_DIRS:
        path = Path(directory) / name
        if path.is_file() and os.access(path, os.X_OK):
            return path
    raise FileNotFoundError(f"brak {name} w katalogach systemowych Poppler")


def tool_env():
    env = {
        "PATH": ":".join(POPPLER_DIRS),
        "LC_ALL": "C",
        "LANG": "C",
        "HF_HUB_OFFLINE": "1",
        "TRANSFORMERS_OFFLINE": "1",
        "PYTHONNOUSERSITE": "1",
    }
    for key in ("HOME", "TMPDIR"):
        if key in os.environ:
            env[key] = os.environ[key]
    return env


def recognize(images, model_path, max_tokens):
    from mlx_vlm import load, generate
    from mlx_vlm.prompt_utils import apply_chat_template

    model, processor = load(model_path)
    prompt = apply_chat_template(processor, model.config, "Text Recognition:", num_images=1)
    pages = []
    for image in images:
        result = generate(model, processor, prompt=prompt, image=str(image),
                          max_tokens=max_tokens, temp=0, verbose=False)
        if result.generation_tokens >= max_tokens:
            raise ValueError("OCR osiągnął limit tokenów; wynik może być ucięty")
        if not result.text.strip():
            raise ValueError("OCR zwrócił pustą stronę")
        pages.append(result.text.strip())
    return pages


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pdf", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--cache-dir", required=True)
    parser.add_argument("--max-pages", type=int, default=10)
    parser.add_argument("--max-tokens", type=int, default=4096)
    args = parser.parse_args()
    os.umask(0o077)
    pdf = Path(args.pdf).resolve(strict=True)
    if not pdf.is_file():
        raise ValueError("PDF OCR musi być zwykłym plikiem")
    if pdf.stat().st_size > MAX_PDF_BYTES:
        raise ValueError(f"PDF przekracza limit {MAX_PDF_BYTES} bajtów")
    model_path = Path(args.model).expanduser().resolve(strict=True)
    if not model_path.is_dir():
        raise ValueError("Model OCR musi być pobrany lokalnie; adapter nie pobiera modeli")
    if not 1 <= args.max_pages <= 100 or not 128 <= args.max_tokens <= 16384:
        raise ValueError("Nieprawidłowy limit stron lub tokenów OCR")
    identity = json.dumps([VERSION, str(model_path), args.max_pages, args.max_tokens, 1600])
    digest = hashlib.sha256(pdf.read_bytes() + identity.encode()).hexdigest()
    cache = Path(args.cache_dir).expanduser()
    cache.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(cache, 0o700)
    target = cache / (digest + ".json")
    if target.exists() and (target.is_symlink() or not target.is_file()):
        raise ValueError("cache OCR nie jest zwykłym plikiem")
    if target.is_file():
        result = json.loads(target.read_text())
        if result.get("version") == VERSION and result.get("text", "").strip():
            os.chmod(target, 0o600)
            print(json.dumps(result, ensure_ascii=False))
            return
    env = tool_env()
    info = subprocess.run([str(poppler_bin("pdfinfo")), str(pdf)], capture_output=True, text=True,
                          timeout=30, check=True, env=env).stdout
    count = next((int(line.split(":", 1)[1].strip()) for line in info.splitlines()
                  if line.startswith("Pages:")), 0)
    if count < 1 or count > args.max_pages:
        raise ValueError(f"PDF ma {count} stron; limit OCR to {args.max_pages}; nie pomijam stron")
    with tempfile.TemporaryDirectory(prefix="lab-ocr-") as tmp:
        prefix = Path(tmp) / "page"
        subprocess.run([str(poppler_bin("pdftoppm")), "-scale-to", "1600", "-png", str(pdf), str(prefix)],
                       capture_output=True, timeout=120, check=True, env=env)
        images = sorted(Path(tmp).glob("page-*.png"), key=lambda p: int(p.stem.split("-")[-1]))
        if len(images) != count:
            raise ValueError("Nie udało się wyrenderować wszystkich stron PDF")
        with contextlib.redirect_stdout(sys.stderr):
            pages = recognize(images, str(model_path), args.max_tokens)
    result = {"version": VERSION, "pages": count, "model": str(model_path),
              "text": "\n\n".join(f"--- strona {i + 1} ---\n{text}" for i, text in enumerate(pages))}
    with tempfile.NamedTemporaryFile(mode="w", dir=cache, delete=False) as handle:
        temp = Path(handle.name)
        json.dump(result, handle, ensure_ascii=False)
        handle.flush()
        os.fsync(handle.fileno())
    try:
        os.chmod(temp, 0o600)
        os.replace(temp, target)
        os.chmod(target, 0o600)
    finally:
        temp.unlink(missing_ok=True)
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        # Nie wypisuj treści dokumentu ani pełnej odpowiedzi subprocess.
        print(f"OCR: {type(exc).__name__}: {exc}", file=sys.stderr)
        sys.exit(1)
