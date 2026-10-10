"""
Live dynamic screen translation module for Wayland ("Game HUD Mode").
Continuously monitors a screen or window region, extracts Japanese text,
and renders containerless, game-native floating subtitles directly above original dialogue.
"""

import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
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
        interval_ms: int = 500,
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
        self.interval_ms = interval_ms
        self.psm = psm
        self.copy_clipboard = copy_clipboard

        self.last_img_hash = ""
        self.last_text_hash = ""
        self.tmp_img = tempfile.mktemp(suffix=".png", prefix="waytrans_live_")

        self.window = None
        self.container = None
        self.active_widgets: List = []

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
        geom = monitor.get_geometry()

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

    def clear_subtitles(self) -> None:
        """Removes all currently displayed floating subtitles."""
        for widget in self.active_widgets:
            self.container.remove(widget)
        self.active_widgets.clear()
        self.container.show_all()
        if hasattr(self, "apply_passthrough"):
            self.apply_passthrough()

    def tick(self) -> bool:
        """Main polling cycle executed reliably on GTK timer."""
        import gi
        from gi.repository import Gtk

        try:
            # 1. Fast frame capture of watched region
            proc = subprocess.run(
                ["grim", "-g", self.scan_geometry, self.tmp_img],
                capture_output=True,
                check=False
            )
            if proc.returncode != 0 or not os.path.exists(self.tmp_img):
                return True

            # 2. Fast perceptual hash comparison to skip idle frames (0% CPU)
            img_hash = compute_image_hash(self.tmp_img)
            if img_hash == self.last_img_hash:
                return True

            self.last_img_hash = img_hash

            # 3. Extract text lines with bounding boxes
            boxes = extract_text_lines_with_boxes(
                self.tmp_img,
                lang=self.source_lang,
                psm=self.psm
            )

            current_text_signature = "|".join(b["text"] for b in boxes)
            if current_text_signature == self.last_text_hash:
                return True

            self.last_text_hash = current_text_signature

            # If screen has no Japanese text, clear subtitles immediately
            if not boxes:
                self.clear_subtitles()
                return True

            # 4. Batch translate all detected text lines
            raw_texts = [b["text"] for b in boxes]
            translated_lines = batch_translate_texts(
                raw_texts,
                source_lang=self.source_lang,
                target_lang=self.target_lang
            )

            # 5. Clear previous widgets and render new floating subtitles directly above original text
            self.clear_subtitles()

            # Relative offset from scan region to screen monitor coordinates
            scan_offset_x = self.scan_x - self.screen_x
            scan_offset_y = self.scan_y - self.screen_y

            for item, trans in zip(boxes, translated_lines):
                if not trans or not trans.strip():
                    continue

                label = Gtk.Label(label=trans)
                label.get_style_context().add_class("hud-subtitle")
                label.set_xalign(0.0)
                label.set_line_wrap(True)
                label.set_max_width_chars(60)

                # Position directly floating above the detected Japanese dialogue line
                # item['top'] is relative to the scan image
                pos_x = scan_offset_x + item["left"]
                pos_y = scan_offset_y + item["top"] - max(int(item["height"] * 0.9), 24)
                pos_y = max(pos_y, 0)

                label.set_selectable(False)

                self.container.put(label, pos_x, pos_y)
                self.active_widgets.append(label)

            self.container.show_all()
            if hasattr(self, "apply_passthrough"):
                self.apply_passthrough()

            # 6. Copy newest dialogue to clipboard if enabled
            if self.copy_clipboard and translated_lines:
                full_text = "\n".join(t for t in translated_lines if t.strip())
                if full_text:
                    subprocess.run(["wl-copy"], input=full_text.encode("utf-8"), check=False)

        except Exception as e:
            print(f"Live HUD translation error: {e}", file=sys.stderr)

        return True  # Keep GLib timer active

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

        # Run first tick immediately and schedule every interval
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
