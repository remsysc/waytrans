"""
Screen capture utilities using Wayland grim and slurp.
"""

import os
import shutil
import subprocess
import tempfile
from typing import Optional, Tuple


class CaptureError(Exception):
    """Raised when screen capture fails or is cancelled."""
    pass


def check_capture_dependencies(need_slurp: bool = True) -> None:
    """Verify that grim (and optionally slurp) are installed."""
    tools = ["grim"]
    if need_slurp:
        tools.append("slurp")
    missing = [tool for tool in tools if not shutil.which(tool)]
    if missing:
        raise CaptureError(
            f"Missing required Wayland capture tools: {', '.join(missing)}. "
            "Please install them via your package manager."
        )


def get_active_monitor_geometry() -> Tuple[int, int, int, int]:
    """
    Detect the geometry (x, y, w, h) of the currently focused monitor in Hyprland or fallback.
    """
    if shutil.which("hyprctl"):
        try:
            import json
            res = subprocess.run(["hyprctl", "monitors", "-j"], capture_output=True, text=True, check=False)
            if res.returncode == 0:
                monitors = json.loads(res.stdout)
                focused = next((m for m in monitors if m.get("focused")), monitors[0] if monitors else None)
                if focused:
                    return int(focused["x"]), int(focused["y"]), int(focused["width"]), int(focused["height"])
        except Exception:
            pass

    # Fallback to Gdk
    try:
        import gi
        gi.require_version('Gdk', '3.0')
        from gi.repository import Gdk
        display = Gdk.Display.get_default()
        if display and display.get_n_monitors() > 0:
            m = display.get_primary_monitor() or display.get_monitor(0)
            geom = m.get_geometry()
            return geom.x, geom.y, geom.width, geom.height
    except Exception:
        pass

    return 0, 0, 1920, 1080


def get_active_window_geometry() -> Optional[Tuple[int, int, int, int]]:
    """
    Detect geometry (x, y, w, h) of the currently focused window in Hyprland.
    """
    if shutil.which("hyprctl"):
        try:
            import json
            res = subprocess.run(["hyprctl", "activewindow", "-j"], capture_output=True, text=True, check=False)
            if res.returncode == 0:
                win = json.loads(res.stdout)
                if win and win.get("mapped") and win.get("size") and win["size"][0] > 0:
                    at = win.get("at", [0, 0])
                    size = win.get("size", [1920, 1080])
                    return int(at[0]), int(at[1]), int(size[0]), int(size[1])
        except Exception:
            pass
    return None



def capture_region(
    border_color: str = "#89b4fa",
    bg_color: str = "#1e1e2e88",
    border_width: int = 2
) -> Optional[Tuple[str, int, int, int, int]]:
    """
    Interactively prompts user to select a screen region using slurp,
    then saves the screenshot using grim.

    Returns:
        (image_path, x, y, width, height) or None if cancelled.
    """
    check_capture_dependencies()

    slurp_cmd = [
        "slurp",
        "-d",
        "-c", border_color,
        "-b", bg_color,
        "-w", str(border_width),
    ]

    try:
        proc = subprocess.run(
            slurp_cmd,
            capture_output=True,
            text=True,
            check=False
        )
    except Exception as e:
        raise CaptureError(f"Failed to run slurp: {e}")

    geometry = proc.stdout.strip()
    if not geometry or proc.returncode != 0:
        return None  # Selection was cancelled by user (e.g. Esc)

    # Geometry output format: "X,Y WxH"
    try:
        coords, dims = geometry.split(" ")
        x_str, y_str = coords.split(",")
        w_str, h_str = dims.split("x")
        x, y, w, h = int(x_str), int(y_str), int(w_str), int(h_str)
    except ValueError:
        raise CaptureError(f"Unexpected geometry format from slurp: '{geometry}'")

    # Create temporary file for screenshot
    tmp_fd, tmp_path = tempfile.mkstemp(suffix=".png", prefix="waytrans_")
    os.close(tmp_fd)

    grim_cmd = ["grim", "-g", geometry, tmp_path]
    try:
        grim_proc = subprocess.run(grim_cmd, capture_output=True, check=False)
        if grim_proc.returncode != 0:
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            raise CaptureError(f"grim failed: {grim_proc.stderr.decode().strip()}")
    except Exception as e:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)
        raise CaptureError(f"Failed to capture region with grim: {e}")

    # Optional image preprocessing if ImageMagick is available
    if shutil.which("magick"):
        try:
            subprocess.run(
                ["magick", tmp_path, "-resize", "200%", "-colorspace", "Gray", tmp_path],
                capture_output=True,
                check=False
            )
        except Exception:
            pass

    return tmp_path, x, y, w, h
