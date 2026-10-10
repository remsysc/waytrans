"""
Live dynamic screen translation module for Wayland ("Game HUD Mode").
Continuously monitors a screen or window region in a background worker,
extracts Japanese text, and renders containerless, game-native floating subtitles
directly above original dialogue in real-time.
"""

import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from typing import Dict, List, Optional, Tuple

from waytrans.capture import (
    capture_region,
    get_active_monitor_geometry,
    get_active_window_geometry,
    CaptureError
)
from waytrans.ocr import extract_text_lines_with_boxes
from waytrans.translator import batch_translate_texts, _TRANSLATION_CACHE
from waytrans.overlay import has_layer_shell_support, notify_fallback

PID_FILE = os.path.expanduser("~/.cache/waytrans/live.pid")

# Regex to detect Japanese / CJK characters (Hiragana, Katakana, Kanji)
CJK_REGEX = re.compile(r'[\u3040-\u309f\u30a0-\u30ff\u4e00-\u9faf]')


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

    notify_fallback("Live HUD Mode", "🛑 Live translation disabled.", title="waytrans")
    return True


def get_image_signature(image_path: str) -> Optional[bytes]:
    """
    Computes a sensitive 64x64 grayscale byte thumbnail.
    Accurately detects dialogue text updates on high-resolution screens.
    """
    try:
        from PIL import Image
        with Image.open(image_path) as img:
            thumb = img.convert('L').resize((64, 64), Image.Resampling.BILINEAR)
            return thumb.tobytes()
    except Exception:
        return None


def has_significant_change(prev_bytes: Optional[bytes], curr_bytes: Optional[bytes], threshold: int = 150) -> bool:
    """
    Returns True if screen pixels changed enough to warrant OCR.
    Threshold of 150 across 4096 pixels easily catches single-character dialogue changes
    while ignoring sub-pixel video rendering noise.
    """
    if prev_bytes is None or curr_bytes is None:
        return True
    if len(prev_bytes) != len(curr_bytes):
        return True

    diff_score = sum(abs(a - b) for a, b in zip(prev_bytes, curr_bytes))
    return diff_score > threshold


class LiveTranslator:
    """
    High-performance, containerless game HUD translation watcher.
    """
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
        copy_clipboard: bool = True,
        fullscreen: bool = True,
        dialogue_zone: Optional[str] = None
    ):
        self.screen_x = x
        self.screen_y = y
        self.screen_w = w
        self.screen_h = h
        self.fullscreen = fullscreen
        self.dialogue_zone = dialogue_zone

        # Calculate scan geometry (can be full monitor or bottom dialogue region)
        if dialogue_zone == "bottom":
            # Scan only bottom 35% of the screen (where games typically put dialogue)
            self.scan_x = self.screen_x
            scan_h = int(self.screen_h * 0.35)
            self.scan_y = self.screen_y + (self.screen_h - scan_h)
            self.scan_w = self.screen_w
            self.scan_h = scan_h
        else:
            self.scan_x = self.screen_x
            self.scan_y = self.screen_y
            self.scan_w = self.screen_w
            self.scan_h = self.screen_h

        self.scan_geometry = f"{self.scan_x},{self.scan_y} {self.scan_w}x{self.scan_h}"
        self.source_lang = source_lang
        self.target_lang = target_lang
        self.interval_sec = max(interval_ms / 1000.0, 0.25)
        self.psm = psm
        self.copy_clipboard = copy_clipboard

        self.last_img_bytes: Optional[bytes] = None
        self.last_text_hash = ""
        self.tmp_img = tempfile.mktemp(suffix=".png", prefix="waytrans_live_")

        self.running = False
        self.worker_thread = None

        self.window = None
        self.container = None
        self.active_widgets: List = []

    def cleanup(self) -> None:
        """Clean up temporary files and PID file."""
        self.running = False
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
        """
        Initializes a 100% transparent GtkLayerShell overlay window covering the target monitor.
        Uses Gtk.Fixed with containerless typography (no cards or boxes).
        """
        import gi
        import cairo
        gi.require_version('Gtk', '3.0')
        gi.require_version('Gdk', '3.0')
        gi.require_version('GtkLayerShell', '0.1')
        from gi.repository import Gtk, Gdk, GtkLayerShell

        Gtk.init()

        display = Gdk.Display.get_default()
        monitor = display.get_monitor_at_point(self.screen_x, self.screen_y)

        self.window = Gtk.Window()
        GtkLayerShell.init_for_window(self.window)
        GtkLayerShell.set_monitor(self.window, monitor)
        GtkLayerShell.set_layer(self.window, GtkLayerShell.Layer.OVERLAY)
        GtkLayerShell.set_namespace(self.window, "waytrans-hud-overlay")

        # Cover the monitored display
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.TOP, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.LEFT, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.RIGHT, True)
        GtkLayerShell.set_anchor(self.window, GtkLayerShell.Edge.BOTTOM, True)

        # Keyboard mode none allows full typing and gaming underneath
        GtkLayerShell.set_keyboard_mode(self.window, GtkLayerShell.KeyboardMode.NONE)

        screen = self.window.get_screen()
        visual = screen.get_rgba_visual()
        if visual:
            self.window.set_visual(visual)

        # Pure containerless styling: transparent background, white bold text with clean outline
        css = b"""
        window {
            background-color: transparent;
        }
        .hud-subtitle {
            color: #ffffff;
            font-family: 'Noto Sans CJK JP', 'Maple Mono NF CN', 'Sans', sans-serif;
            font-size: 16px;
            font-weight: 800;
            letter-spacing: 0.5px;
            background-color: transparent;
            text-shadow:
                2px 2px 0px #000000,
                -2px -2px 0px #000000,
                2px -2px 0px #000000,
                -2px 2px 0px #000000,
                0px 2px 0px #000000,
                0px -2px 0px #000000,
                2px 0px 0px #000000,
                -2px 0px 0px #000000;
        }
        """
        css_provider = Gtk.CssProvider()
        css_provider.load_from_data(css)
        Gtk.StyleContext.add_provider_for_screen(
            screen, css_provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )

        self.container = Gtk.Fixed()
        self.window.add(self.container)
        self.window.realize()

        def apply_passthrough(*args):
            reg = cairo.Region()
            self.window.input_shape_combine_region(reg)
            gdk_w = self.window.get_window()
            if gdk_w:
                gdk_w.input_shape_combine_region(reg, 0, 0)

        self.apply_passthrough = apply_passthrough
        self.window.connect("map", apply_passthrough)
        self.apply_passthrough()
        self.window.show_all()

    def update_subtitles_on_main_thread(self, items_to_render: List[Dict[str, any]]) -> None:
        """
        Runs on the GTK main thread via GLib.idle_add.
        Clears old widgets and renders new floating subtitle labels.
        """
        import gi
        from gi.repository import Gtk

        for widget in self.active_widgets:
            self.container.remove(widget)
        self.active_widgets.clear()

        scan_offset_x = self.scan_x - self.screen_x
        scan_offset_y = self.scan_y - self.screen_y

        for item in items_to_render:
            trans = item["trans"]
            label = Gtk.Label(label=trans)
            label.get_style_context().add_class("hud-subtitle")
            label.set_xalign(0.0)
            label.set_line_wrap(True)
            label.set_max_width_chars(60)
            label.set_selectable(False)

            # Position directly floating above the detected Japanese dialogue line
            pos_x = scan_offset_x + item["left"]
            pos_y = scan_offset_y + item["top"] - max(int(item["height"] * 0.9), 24)
            pos_y = max(pos_y, 0)

            self.container.put(label, pos_x, pos_y)
            self.active_widgets.append(label)

        self.container.show_all()
        if hasattr(self, "apply_passthrough"):
            self.apply_passthrough()

    def background_scan_loop(self) -> None:
        """
        Background worker thread executing capture, OCR, and translation
        without blocking the GTK event loop or stuttering game frame rates.
        """
        import gi
        from gi.repository import GLib

        while self.running:
            try:
                # 1. Fast frame capture of watched region
                proc = subprocess.run(
                    ["grim", "-g", self.scan_geometry, self.tmp_img],
                    capture_output=True,
                    check=False
                )
                if proc.returncode != 0 or not os.path.exists(self.tmp_img):
                    time.sleep(self.interval_sec)
                    continue

                # 2. Fast sensitive signature check to skip completely static scenes
                img_bytes = get_image_signature(self.tmp_img)
                if img_bytes and self.last_img_bytes and not has_significant_change(self.last_img_bytes, img_bytes):
                    time.sleep(self.interval_sec)
                    continue

                self.last_img_bytes = img_bytes

                # 3. Extract text lines with bounding boxes
                boxes = extract_text_lines_with_boxes(
                    self.tmp_img,
                    lang=self.source_lang,
                    psm=self.psm
                )

                # Filter lines: only process lines that actually contain CJK characters for Japanese mode
                if self.source_lang.lower() in ("ja", "jp", "jpn"):
                    boxes = [b for b in boxes if CJK_REGEX.search(b["text"])]

                current_text_signature = "|".join(b["text"] for b in boxes)
                if current_text_signature == self.last_text_hash:
                    time.sleep(self.interval_sec)
                    continue

                self.last_text_hash = current_text_signature

                # If no Japanese text remains on screen, clear subtitles immediately
                if not boxes:
                    GLib.idle_add(self.update_subtitles_on_main_thread, [])
                    time.sleep(self.interval_sec)
                    continue

                # 4. Batch translate all detected text lines
                raw_texts = [b["text"] for b in boxes]
                translated_lines = batch_translate_texts(
                    raw_texts,
                    source_lang=self.source_lang,
                    target_lang=self.target_lang
                )

                items_to_render = []
                for b, trans in zip(boxes, translated_lines):
                    if trans and trans.strip():
                        items_to_render.append({
                            "left": b["left"],
                            "top": b["top"],
                            "height": b["height"],
                            "width": b["width"],
                            "trans": trans
                        })

                # Schedule UI update on GTK main loop
                GLib.idle_add(self.update_subtitles_on_main_thread, items_to_render)

                # 5. Copy dialogue to clipboard if enabled
                if self.copy_clipboard and translated_lines:
                    full_text = "\n".join(t for t in translated_lines if t.strip())
                    if full_text:
                        subprocess.run(["wl-copy"], input=full_text.encode("utf-8"), check=False)

            except Exception as e:
                print(f"Background HUD loop error: {e}", file=sys.stderr)

            time.sleep(self.interval_sec)

    def run(self) -> None:
        """Starts the live monitoring loop with background scanning."""
        import gi
        gi.require_version('Gtk', '3.0')
        from gi.repository import Gtk

        with open(get_pid_file(), "w") as f:
            f.write(str(os.getpid()))

        def handle_signal(sig, frame):
            self.cleanup()
            Gtk.main_quit()
            sys.exit(0)

        signal.signal(signal.SIGTERM, handle_signal)
        signal.signal(signal.SIGINT, handle_signal)

        self.setup_ui()

        # Start non-blocking background scanner thread
        self.running = True
        self.worker_thread = threading.Thread(target=self.background_scan_loop, daemon=True)
        self.worker_thread.start()

        try:
            Gtk.main()
        finally:
            self.cleanup()


def toggle_live(
    source_lang: str = "ja",
    target_lang: str = "en",
    interval: float = 0.4,
    psm: int = 6,
    copy_clipboard: bool = True,
    fullscreen: bool = True,
    dialogue_zone: Optional[str] = None
) -> None:
    """
    Toggles dynamic live translation mode on or off.
    By default, activates Game HUD Mode across fullscreen or active focused window.
    """
    if is_live_running():
        stop_live()
        print("Live translation stopped.")
        return

    if not has_layer_shell_support():
        notify_fallback("Live Mode Error", "Live mode requires gtk-layer-shell.", title="waytrans")
        return

    if fullscreen:
        # Detect active monitor geometry
        mx, my, mw, mh = get_active_monitor_geometry()
        x, y, w, h = mx, my, mw, mh
        notify_fallback(
            "Game HUD Mode Active",
            f"🎮 Monitoring screen ({w}x{h}). Subtitles will float above dialogue.\nPress Super+Alt+L to stop.",
            title="waytrans"
        )
    else:
        # Fallback to interactive region selection
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

        if w < 25 or h < 15:
            notify_fallback("Live Mode", "Selected region was too small. Please drag a box around the text.", title="waytrans")
            return

        notify_fallback(
            "Live Mode Active",
            f"🟢 Watching {w}x{h} region. Press Super+Alt+L to stop.",
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
        copy_clipboard=copy_clipboard,
        fullscreen=fullscreen,
        dialogue_zone=dialogue_zone
    )
    watcher.run()
