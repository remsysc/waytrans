"""
Translation module using translate-shell or fallback services with batching and caching.
"""

import re
import shutil
import subprocess
from typing import Dict, List, Optional

# In-memory translation cache (cleared per session)
_TRANSLATION_CACHE: Dict[str, str] = {}


class TranslationError(Exception):
    """Raised when translation operations fail."""
    pass


def check_translator_dependencies() -> None:
    """Verify that translate-shell (trans) is installed."""
    if not shutil.which("trans"):
        raise TranslationError(
            "translate-shell ('trans') is not installed. Please install it:\n"
            "  Arch/CachyOS: sudo pacman -S translate-shell\n"
            "  Fedora:       sudo dnf install translate-shell\n"
            "  Debian/Ubuntu:sudo apt install translate-shell"
        )


def translate_text(
    text: str,
    source_lang: str = "ja",
    target_lang: str = "en"
) -> str:
    """
    Translates a single input string using translate-shell with caching.
    """
    cleaned = text.strip()
    if not cleaned:
        return ""

    cache_key = f"{source_lang}:{target_lang}:{cleaned}"
    if cache_key in _TRANSLATION_CACHE:
        return _TRANSLATION_CACHE[cache_key]

    check_translator_dependencies()

    cmd = [
        "trans",
        "-brief",
        "-no-ansi",
        "-e", "google",
        f"{source_lang}:{target_lang}",
        cleaned
    ]

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=False
        )
    except Exception as e:
        raise TranslationError(f"Failed to execute translate-shell: {e}")

    if proc.returncode != 0:
        err = proc.stderr.strip() or "Unknown error"
        raise TranslationError(f"Translation failed: {err}")

    result = proc.stdout.strip()
    _TRANSLATION_CACHE[cache_key] = result
    return result


def batch_translate_texts(
    texts: List[str],
    source_lang: str = "ja",
    target_lang: str = "en",
    batch_size: int = 30
) -> List[str]:
    """
    Translates a list of strings in a single batched network call using a delimiter.
    Utilizes an in-memory cache to avoid redundant network requests.
    """
    if not texts:
        return []

    check_translator_dependencies()

    results: List[str] = [""] * len(texts)
    missing_indices: List[int] = []
    missing_texts: List[str] = []

    # 1. Lookup cached entries
    for i, t in enumerate(texts):
        cleaned = t.strip()
        if not cleaned:
            continue
        cache_key = f"{source_lang}:{target_lang}:{cleaned}"
        if cache_key in _TRANSLATION_CACHE:
            results[i] = _TRANSLATION_CACHE[cache_key]
        else:
            missing_indices.append(i)
            missing_texts.append(cleaned)

    # If all items were cached, return immediately with zero network latency
    if not missing_texts:
        return results

    # 2. Batch missing entries in chunks using ' ||| ' delimiter
    delimiter = " ||| "
    for chunk_start in range(0, len(missing_texts), batch_size):
        chunk_texts = missing_texts[chunk_start:chunk_start + batch_size]
        chunk_indices = missing_indices[chunk_start:chunk_start + batch_size]

        payload = delimiter.join(chunk_texts)
        cmd = [
            "trans",
            "-brief",
            "-no-ansi",
            "-e", "google",
            f"{source_lang}:{target_lang}",
            payload
        ]

        try:
            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            if proc.returncode == 0:
                raw_out = proc.stdout.strip()
                translated_parts = [p.strip() for p in re.split(r'\s*\|\|\|\s*', raw_out)]

                # Match translated parts with chunk texts
                matched_count = min(len(translated_parts), len(chunk_texts))
                for idx, orig_text, trans_text in zip(chunk_indices[:matched_count], chunk_texts[:matched_count], translated_parts[:matched_count]):
                    results[idx] = trans_text
                    _TRANSLATION_CACHE[f"{source_lang}:{target_lang}:{orig_text}"] = trans_text

                # If delimiter count mismatched, fallback to individual translation for remaining
                if len(translated_parts) < len(chunk_texts):
                    for idx, orig_text in zip(chunk_indices[matched_count:], chunk_texts[matched_count:]):
                        single_t = translate_text(orig_text, source_lang, target_lang)
                        results[idx] = single_t
            else:
                for idx, orig_text in zip(chunk_indices, chunk_texts):
                    results[idx] = translate_text(orig_text, source_lang, target_lang)

        except Exception:
            for idx, orig_text in zip(chunk_indices, chunk_texts):
                try:
                    results[idx] = translate_text(orig_text, source_lang, target_lang)
                except Exception:
                    results[idx] = orig_text

    return results
