"""
OCR module using Tesseract with automatic CJK handling and model management.
"""

import os
import re
import shutil
import subprocess
import urllib.request
from typing import Dict, List, Optional


class OCRError(Exception):
    """Raised when OCR operations fail."""
    pass


LANG_MAP: Dict[str, str] = {
    "ja": "jpn+jpn_vert",
    "jp": "jpn+jpn_vert",
    "jpn": "jpn+jpn_vert",
    "zh": "chi_sim+chi_tra",
    "zh-cn": "chi_sim",
    "zh-tw": "chi_tra",
    "ko": "kor",
    "kr": "kor",
    "ru": "rus",
    "de": "deu",
    "fr": "fra",
    "es": "spa",
}

TESSDATA_URLS = {
    "jpn": "https://github.com/tesseract-ocr/tessdata_best/raw/main/jpn.traineddata",
    "jpn_vert": "https://github.com/tesseract-ocr/tessdata_best/raw/main/jpn_vert.traineddata",
}


def get_tessdata_dir() -> str:
    """Return the user local tessdata directory."""
    local_dir = os.path.expanduser("~/.local/share/tessdata")
    os.makedirs(local_dir, exist_ok=True)
    return local_dir


def check_ocr_dependencies() -> None:
    """Verify that tesseract is installed."""
    if not shutil.which("tesseract"):
        raise OCRError(
            "Tesseract is not installed. Please install it with your package manager:\n"
            "  Arch/CachyOS: sudo pacman -S tesseract\n"
            "  Fedora:       sudo dnf install tesseract\n"
            "  Debian/Ubuntu:sudo apt install tesseract-ocr"
        )


def setup_japanese_models() -> None:
    """Download high-accuracy Japanese trained data into ~/.local/share/tessdata."""
    target_dir = get_tessdata_dir()
    print(f"Setting up high-accuracy Japanese OCR models in {target_dir}...")

    for name, url in TESSDATA_URLS.items():
        dest = os.path.join(target_dir, f"{name}.traineddata")
        if os.path.exists(dest) and os.path.getsize(dest) > 1000000:
            print(f"  ✓ {name}.traineddata already exists ({os.path.getsize(dest) // 1024} KB)")
            continue

        print(f"  ⬇ Downloading {name}.traineddata from GitHub (tessdata_best)...")
        try:
            req = urllib.request.Request(
                url,
                headers={"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) waytrans"}
            )
            with urllib.request.urlopen(req) as response, open(dest, "wb") as out_file:
                shutil.copyfileobj(response, out_file)
            print(f"  ✓ Successfully downloaded {name}.traineddata")
        except Exception as e:
            raise OCRError(f"Failed to download {name}.traineddata: {e}")

    print("Model setup complete!")


def get_tesseract_languages() -> List[str]:
    """Retrieve available Tesseract languages."""
    env = os.environ.copy()
    local_dir = get_tessdata_dir()
    if os.path.exists(local_dir):
        env["TESSDATA_PREFIX"] = local_dir

    try:
        proc = subprocess.run(
            ["tesseract", "--list-langs"],
            capture_output=True,
            text=True,
            env=env,
            check=False
        )
        lines = [line.strip() for line in proc.stdout.splitlines()[1:] if line.strip()]
        return lines
    except Exception:
        return []


def run_ocr(
    image_path: str,
    lang: str = "ja",
    psm: int = 6
) -> str:
    """
    Extract text from the given image using Tesseract.
    Cleans up whitespaces for CJK languages.
    """
    check_ocr_dependencies()

    tess_lang = LANG_MAP.get(lang.lower(), lang)

    # Determine TESSDATA_PREFIX
    env = os.environ.copy()
    local_dir = get_tessdata_dir()
    if os.path.exists(os.path.join(local_dir, "jpn.traineddata")):
        env["TESSDATA_PREFIX"] = local_dir

    cmd = [
        "tesseract",
        image_path,
        "stdout",
        "-l", tess_lang,
        "--psm", str(psm)
    ]

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            env=env,
            check=False
        )
    except Exception as e:
        raise OCRError(f"Failed to execute tesseract: {e}")

    raw_text = proc.stdout

    # Post-process extracted text
    if lang.lower() in ("ja", "jp", "jpn", "zh", "chi_sim", "chi_tra"):
        # For CJK, remove spaces between CJK characters introduced by OCR
        cleaned = re.sub(r'[\r\t]', '', raw_text)
        # Remove whitespace between Japanese/Chinese glyphs
        cleaned = re.sub(r'([一-龯ぁ-んァ-ヶ])\s+([一-龯ぁ-んァ-ヶ])', r'\1\2', cleaned)
        cleaned = cleaned.replace(" ", "").strip()
    else:
        # For Latin and other languages, collapse extra spaces and clean lines
        lines = [line.strip() for line in raw_text.splitlines() if line.strip()]
        cleaned = " ".join(lines)

    return cleaned


def extract_text_lines_with_boxes(
    image_path: str,
    lang: str = "ja",
    psm: int = 6
) -> List[Dict[str, any]]:
    """
    Extracts text lines along with their exact pixel bounding boxes (left, top, width, height)
    using Tesseract's TSV mode.
    """
    check_ocr_dependencies()

    tess_lang = LANG_MAP.get(lang.lower(), lang)

    env = os.environ.copy()
    local_dir = get_tessdata_dir()
    if os.path.exists(os.path.join(local_dir, "jpn.traineddata")):
        env["TESSDATA_PREFIX"] = local_dir

    cmd = [
        "tesseract",
        image_path,
        "stdout",
        "-l", tess_lang,
        "--psm", str(psm),
        "-c", "tessedit_create_tsv=1"
    ]

    try:
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            env=env,
            check=False
        )
    except Exception as e:
        raise OCRError(f"Failed to execute tesseract TSV: {e}")

    lines: List[Dict[str, any]] = []
    curr_line: Optional[Dict[str, any]] = None

    for row in proc.stdout.splitlines():
        parts = row.split('\t')
        if len(parts) < 12:
            continue
        level = parts[0]
        if level == '4':  # Line level
            if curr_line and curr_line["text"]:
                lines.append(curr_line)
            curr_line = {
                "left": int(parts[6]),
                "top": int(parts[7]),
                "width": int(parts[8]),
                "height": int(parts[9]),
                "text": []
            }
        elif level == '5' and curr_line is not None:  # Word level
            word = parts[11].strip()
            if word:
                curr_line["text"].append(word)

    if curr_line and curr_line["text"]:
        lines.append(curr_line)

    results: List[Dict[str, any]] = []
    is_cjk = lang.lower() in ("ja", "jp", "jpn", "zh", "chi_sim", "chi_tra")

    for l in lines:
        if is_cjk:
            cleaned = "".join(l["text"])
            cleaned = re.sub(r'[\r\t\n]', '', cleaned)
            cleaned = re.sub(r'([一-龯ぁ-んァ-ヶ])\s+([一-龯ぁ-んァ-ヶ])', r'\1\2', cleaned)
            cleaned = cleaned.replace(" ", "").strip()
        else:
            cleaned = " ".join(l["text"]).strip()

        if cleaned:
            results.append({
                "text": cleaned,
                "left": l["left"],
                "top": l["top"],
                "width": l["width"],
                "height": l["height"]
            })

    # If uniform block (psm=6) found no text on complex layout, retry with auto segmentation (psm=3)
    if not results and psm != 3:
        return extract_text_lines_with_boxes(image_path, lang=lang, psm=3)

    return results
