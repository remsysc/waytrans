"""
Live dynamic screen translation module for Wayland.
Watches a selected screen region, detects text lines using Tesseract TSV bounding boxes,
and projects translated English text directly on top of the original words in real-time.
"""

import hashlib
import os
import signal
import subprocess
import sys
import tempfile
from typing import Dict, List, Optional

from waytrans.capture import capture_region, CaptureError
from waytrans.ocr import extract_text_lines_with_boxes
from waytrans.translator import translate_text
from waytrans.overlay import has_layer_shell_support, notify_fallback

PID_FILE = os.path.expanduser("~/.cache/waytrans/live.pid")


def get_pid_file() -> str:
    """Return and ensure directory for the PID file."""
    os.makedirs(os.path.dirname(PID_FILE), exist_ok=True)
    return PID_FILE


def is_live_running() -> Optional[int]:
    """Check if a live translation daemon is currently active."""
    pid_path = get_pid_file()
    if not os.path.exists(pid_path):
        return None

    try:
        with open(pid_path, "r") as f:
            pid = int(f.read().strip())
        os.kill(pid, 0)
        return pid
    except (ValueError, OSError):
        if os.path.exists(pid_path):
            try:
                os.remove(pid_path)
            except OSError:
                pass
        return None


def stop_live() -> bool:
    """Stop any running live translation process."""
    pid = is_live_running()
    if not pid:
        return False

    try:
        os.kill(pid, signal.SIGTERM)
    except OSError:
        pass

    pid_path = get_pid_file()
    if os.path.exists(pid_path):
        try:
            os.remove(pid_path)
        except OSError:
            pass

    notify_fallback("Live Mode", "🛑 Live translation disabled.", title="waytrans")
    return True


def compute_image_hash(image_path: str) -> str:
    """Computes a fast perceptual hash (aHash) or MD5."""
    try:
        from PIL import Image
        with Image.open(image_path) as img:
            thumb = img.convert('L').resize((16, 16), Image.Resampling.BILINEAR)
            pixels = list(thumb.getdata())
            avg = sum(pixels) / max(len(pixels), 1)
            bits = "".join("1" if p > avg else "0" for p in pixels)
            return hex(int(bits, 2))
    except Exception:
        hasher = hashlib.md5()
        with open(image_path, "rb") as f:
            hasher.update(f.read())
        return hasher.hexdigest()


class LiveTranslator:
    def __init__(
        self,
        x: int,
        y: int,
        w: int,
        h: int,
        source_lang: str = "ja",
        target_lang: str = "en",
        interval_ms: int = 400,
        psm: int = 6,
        copy_clipboard: bool = True
    ):
        self.x = x
        self.y = y
        self.w = w
        self.h = h
        self.geometry = f"{x},{y} {w}x{h}"
        self.source_lang = source_lang
        self.target_lang = target_lang
        self.interval_ms = interval_ms
        self.psm = psm
        self.copy_clipboard = copy_clipboard

        self.last_img_hash = ""
        self.last_combined_text = ""
        self.tmp_img = tempfile.mktemp(suffix=".png", prefix="waytrans_live_")

        self.window = None
        self.fixed = None
        self.active_widgets: List[any] = []

    def cleanup(self) -> None:
        """Clean up temporary files and PID file."""
        if os.path.exists(self.tmp_img):
            try:
                os.remove(self.tmp_img)
            except OSError:
                pass
        pid_path = get_pid_file()
        if os.path.exists(pid_path):
            try:
                os.remove(pid_path)
            except OSError:
                pass

    def setup_ui(self) -> None:
        """Initializes the GtkLayerShell overlay window anchored over the watched region."""
        import gi
        import cairo
        gi.require_version('Gtk', '3.0')
        gi.require_version('Gdk', '3.0')
        gi.require_version('GtkLayerShell', '0.1')
        from gi.repository import Gtk, Gdk, GtkLayerShell

        Gtk.init()

        display = Gdk.Display.get_default()
        monitor = display.get_monitor_at_point(self.x, self.y)
        geom = monitor.get_geometry()
        local_x = max(0, self.x - geom.x)
        local_y = max(0, self.y - geom.y)

        self.window = Gtk.Window()
        GtkLayerShell.init_for_window(self.window)
        GtkLayerShell.set_monitor(self.window, monitor)
        GtkLayerShell.set_layer(self.window, GtkLayerShell.Layer.OVERLAY)
        GtkLayerShell.set_namespace(self.window, "waytrans-live-overlay")

        # Anchor window to exact region
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.TOP, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.LEFT, True)
        GtkLayerShell.set_margin(self.window, GtkLayerShell.Edge.LEFT, local_x)
        GtkLayerShell.set_margin(self.window, GtkLayerShell.Edge.TOP, local_y)

        # Keyboard mode none allows unrestricted typing in apps underneath
        GtkLayerShell.set_keyboard_mode(self.window, GtkLayerShell.KeyboardMode.NONE)

        # Set window size to match the region
        self.window.set_default_size(self.w, self.h)

        screen = self.window.get_screen()
        visual = screen.get_rgba_visual()
        if visual:
            self.window.set_visual(visual)

        # CSS Styling - Window itself is completely transparent
        css = b"""
        window {
            background-color: transparent;
        }
        .text-mask {
            background-color: rgba(16, 16, 24, 0.94);
            border: 1px solid #89b4fa;
            border-radius: 4px;
            padding: 2px 6px;
        }
        .translated-label {
            color: #ffffff;
            font-weight: 700;
            letter-spacing: 0.2px;
        }
        """
        css_provider = Gtk.CssProvider()
        css_provider.load_from_data(css)
        Gtk.StyleContext.add_provider_for_screen(
            screen, css_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )

        self.fixed = Gtk.Fixed()
        self.window.add(self.fixed)
        self.window.realize()

        # Input shape pass-through so clicks pass directly to the game/manga
        self.window.input_shape_combine_region(cairo.Region())
        self.window.show_all()

    def clear_overlay(self) -> None:
        """Removes all translated text masks from the overlay."""
        if self.fixed:
            for child in self.fixed.get_children():
                self.fixed.remove(child)

    def tick(self) -> bool:
        """Polls screen, parses exact text bounding boxes, and updates in-place masks."""
        from gi.repository import Gtk

        try:
            # 1. Capture watched region
            proc = subprocess.run(
                ["grim", "-g", self.geometry, self.tmp_img],
                capture_output=True,
                check=False
            )
            if proc.returncode != 0 or not os.path.exists(self.tmp_img):
                return True

            # 2. Check if screen region changed
            img_hash = compute_image_hash(self.tmp_img)
            if img_hash == self.last_img_hash:
                return True

            self.last_img_hash = img_hash

            # 3. Extract text lines and their pixel bounding boxes
            boxes = extract_text_lines_with_boxes(self.tmp_img, lang=self.source_lang, psm=self.psm)
            combined_text = " ".join(b["text"] for b in boxes)

            if combined_text == self.last_combined_text:
                return True

            self.last_combined_text = combined_text

            # 4. If no text detected on screen, clear overlay
            if not boxes:
                self.clear_overlay()
                return True

            # 5. Clear previous in-place boxes and render new translations
            self.clear_overlay()

            full_translations = []

            for item in boxes:
                raw_text = item["text"]
                if not raw_text.strip():
                    continue

                translated = translate_text(
                    raw_text,
                    source_lang=self.source_lang,
                    target_lang=self.target_lang
                )
                if not translated:
                    continue

                full_translations.append(translated)

                # Determine dynamic font size based on text height
                line_h = max(18, item["height"])
                font_size = max(11, min(24, int(line_h * 0.65)))

                # Container covering the original text
                box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL)
                box.get_style_context().add_class("text-mask")

                lbl = Gtk.Label()
                lbl.set_text(translated)
                lbl.get_style_context().add_class("translated-label")
                lbl.set_xalign(0.0)

                # Dynamic font styling
                font_css = f".translated-label {{ font-size: {font_size}px; }}".encode("utf-8")
                lbl_provider = Gtk.CssProvider()
                lbl_provider.load_from_data(font_css)
                lbl.get_style_context().add_provider(lbl_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

                box.pack_start(lbl, False, False, 0)

                # Ensure box covers the line height and accommodates English length
                est_w = max(item["width"], len(translated) * int(font_size * 0.65) + 16)
                box.set_size_request(min(est_w, self.w), line_h)

                # Place directly on top of the original words
                pos_x = max(0, min(item["left"], self.w - 20))
                pos_y = max(0, min(item["top"], self.h - 15))
                self.fixed.put(box, pos_x, pos_y)

            self.fixed.show_all()

            # 6. Copy translated text to clipboard
            if self.copy_clipboard and full_translations:
                joined = "\n".join(full_translations)
                subprocess.run(["wl-copy"], input=joined.encode("utf-8"), check=False)

        except Exception as e:
            print(f"Live translation tick error: {e}", file=sys.stderr)

        return True

    def run(self) -> None:
        """Starts the live monitoring loop."""
        import gi
        gi.require_version('Gtk', '3.0')
        from gi.repository import Gtk, GLib

        with open(get_pid_file(), "w") as f:
            f.write(str(os.getpid()))

        def handle_signal(sig, frame):
            self.cleanup()
            Gtk.main_quit()
            sys.exit(0)

        signal.signal(signal.SIGTERM, handle_signal)
        signal.signal(signal.SIGINT, handle_signal)

        self.setup_ui()
        self.tick()
        GLib.timeout_add(self.interval_ms, self.tick)

        try:
            Gtk.main()
        finally:
            self.cleanup()


def toggle_live(
    source_lang: str = "ja",
    target_lang: str = "en",
    interval: float = 0.5,
    psm: int = 6,
    copy_clipboard: bool = True
) -> None:
    """Toggles dynamic live translation mode on or off."""
    if is_live_running():
        stop_live()
        print("Live translation stopped.")
        return

    if not has_layer_shell_support():
        notify_fallback("Live Mode Error", "Live mode requires gtk-layer-shell.", title="waytrans")
        return

    notify_fallback("Live Mode", "Select region to watch...", title="waytrans")

    try:
        captured = capture_region(border_color="#a6e3a1", bg_color="#1e1e2e66")
    except CaptureError as e:
        print(f"Capture error: {e}", file=sys.stderr)
        return

    if not captured:
        print("Selection cancelled.")
        return

    image_path, x, y, w, h = captured
    if os.path.exists(image_path):
        try:
            os.remove(image_path)
        except OSError:
            pass

    notify_fallback(
        "Live Mode",
        f"🟢 Live translation active ({w}x{h}). Press toggle hotkey again to stop.",
        title="waytrans"
    )

    watcher = LiveTranslator(
        x=x,
        y=y,
        w=w,
        h=h,
        source_lang=source_lang,
        target_lang=target_lang,
        interval_ms=int(interval * 1000),
        psm=psm,
        copy_clipboard=copy_clipboard
    )
    watcher.run()
