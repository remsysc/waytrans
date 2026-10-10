"""
Command-line interface and main controller for waytrans.
"""

import argparse
import os
import shutil
import subprocess
import sys

from waytrans import __version__
from waytrans.capture import capture_region, CaptureError
from waytrans.ocr import run_ocr, setup_japanese_models, OCRError
from waytrans.translator import translate_text, TranslationError
from waytrans.overlay import display_overlay, notify_fallback, has_layer_shell_support
from waytrans.live import toggle_live, stop_live


def copy_to_clipboard(text: str) -> bool:
    """Copy string to Wayland clipboard using wl-copy."""
    if shutil.which("wl-copy"):
        try:
            subprocess.run(["wl-copy"], input=text.encode("utf-8"), check=False)
            return True
        except Exception:
            return False
    return False


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="waytrans",
        description="Instant in-place screen OCR translator for Wayland desktops (Hyprland, Sway, River, etc.)."
    )

    parser.add_argument(
        "-s", "--source",
        default="ja",
        help="Source language for OCR and translation (default: ja)."
    )
    parser.add_argument(
        "-t", "--target",
        default="en",
        help="Target language for translation (default: en)."
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=10,
        help="Overlay duration in seconds before auto-dismiss (default: 10, 0 = stay until clicked)."
    )
    parser.add_argument(
        "--psm",
        type=int,
        default=6,
        help="Tesseract Page Segmentation Mode (default: 6)."
    )
    parser.add_argument(
        "--no-overlay",
        action="store_true",
        help="Disable in-place floating overlay (desktop notification only)."
    )
    parser.add_argument(
        "--no-clipboard",
        action="store_true",
        help="Disable copying translation to clipboard."
    )
    parser.add_argument(
        "--notify",
        action="store_true",
        help="Send a desktop notification in addition to the on-screen overlay."
    )
    parser.add_argument(
        "-l", "--live",
        action="store_true",
        help="Toggle dynamic live translation mode on or off."
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=0.5,
        help="Polling interval in seconds for live translation (default: 0.5)."
    )
    parser.add_argument(
        "--stop-live",
        action="store_true",
        help="Stop any currently running live translation session."
    )
    parser.add_argument(
        "--setup-models",
        action="store_true",
        help="Download high-accuracy Japanese OCR models into ~/.local/share/tessdata."
    )
    parser.add_argument(
        "-v", "--version",
        action="version",
        version=f"%(prog)s {__version__}"
    )

    args = parser.parse_args()

    # Handle stop live
    if args.stop_live:
        if stop_live():
            print("Live translation stopped.")
        else:
            print("No active live translation session found.")
        sys.exit(0)

    # Handle live toggle
    if args.live:
        toggle_live(
            source_lang=args.source,
            target_lang=args.target,
            interval=args.interval,
            psm=args.psm,
            copy_clipboard=not args.no_clipboard
        )
        sys.exit(0)

    # Handle model installation helper
    if args.setup_models:
        try:
            setup_japanese_models()
            sys.exit(0)
        except Exception as e:
            print(f"Error setting up models: {e}", file=sys.stderr)
            sys.exit(1)

    # 1. Capture screen region
    try:
        captured = capture_region()
    except CaptureError as e:
        print(f"Capture error: {e}", file=sys.stderr)
        notify_fallback("Capture Error", str(e), title="waytrans")
        sys.exit(1)

    if not captured:
        # User cancelled region selection (Esc)
        sys.exit(0)

    image_path, x, y, w, h = captured

    try:
        # 2. Extract text via OCR
        extracted_text = run_ocr(image_path, lang=args.source, psm=args.psm)

        if not extracted_text:
            notify_fallback("", "No text detected in selected region.", title="waytrans")
            sys.exit(0)

        # 3. Translate
        translated_text = translate_text(
            extracted_text,
            source_lang=args.source,
            target_lang=args.target
        )

        if not translated_text:
            notify_fallback(extracted_text, "Translation returned empty result.", title="waytrans")
            sys.exit(0)

        # 4. Copy to clipboard
        if not args.no_clipboard:
            copy_to_clipboard(translated_text)

        # 5. Send desktop notification for immediate visual confirmation
        notify_fallback(extracted_text, translated_text, title=f"waytrans [{args.source}➔{args.target}]")

        # 6. Display in-place overlay if supported
        if not args.no_overlay and has_layer_shell_support():
            display_overlay(
                x=x,
                y=y,
                w=w,
                h=h,
                source_text=extracted_text,
                translated_text=translated_text,
                timeout_ms=args.timeout * 1000
            )

    except (OCRError, TranslationError) as e:
        print(f"Error: {e}", file=sys.stderr)
        notify_fallback("Error", str(e), title="waytrans")
        sys.exit(1)
    finally:
        # Clean up temporary screenshot
        if os.path.exists(image_path):
            try:
                os.remove(image_path)
            except OSError:
                pass


if __name__ == "__main__":
    main()
