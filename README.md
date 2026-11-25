# waytrans

> **Instant in-place screen OCR translator in Rust for Wayland (and cross-platform desktops).**  
> Snip any foreign text or toggle Live HUD mode while playing Visual Novels / JRPGs, reading manga, or watching Japanese media, and see translations rendered **directly on top of or above the original words**.

---

## Overview

`waytrans` is built as a modular Rust Cargo workspace using native Wayland protocols (`zwlr_layer_shell_v1` via `smithay-client-toolkit`, `tiny-skia`, and `cosmic-text`) and persistent in-memory Tesseract FFI (`libloading` against `libtesseract`):

1. **In-Memory Capture**: Captures the active monitor or selected region via `grim` stdout PPM (zero temp files).
2. **Pre-processing & Layout**: Dual-polarity contrast enhancement, active overlay masking, horizontal/vertical Japanese OCR (`jpn+jpn_vert`), and smart dialogue vs. menu choice clustering.
3. **Context-Aware Translation**: Supports free Google Translate as well as LLM engines (local Ollama, Groq, OpenRouter, or Gemini) with persistent caching and speaker/glossary awareness.
4. **Click-Through Layer-Shell HUD**: Renders crisp hardware-independent subtitle pills or floating captions directly over the game with empty input shape (`wl_region`) so clicks and controls pass straight through.
5. **System Tray Control**: Live mode exposes a StatusNotifierItem (`ksni`) tray icon for 1-click Pause/Resume, overlay style switching (`In-Place` / `Above`), and Quit.

---

## Features

- **Dynamic Live Translation (Toggle Mode)**: Continuously watches dialogue boxes and stacked choice menus, detects screen changes via fast perceptual hashing, and updates translations automatically.
- **100% Click-Through HUD**: Mouse clicks pass directly through the Wayland overlay to your game or window.
- **Two Overlay Styles**:
  - `inplace` *(default)*: Dark pill anchored over the original Japanese text.
  - `above`: Floating caption above each line for Japanese reading practice.
- **Visual Novel & JRPG Smart Clustering**: Separates speaker name banners and stacked menu choices from multi-line dialogue paragraphs and avoids cascading pills onto adjacent rows.
- **AI & Offline LLM Support**: Plug in local Ollama (e.g. `qwen2.5:3b`) or any OpenAI-compatible / Gemini endpoint, with automatic fallback and persistent cache (`~/.cache/waytrans/translations.json`).
- **Custom Glossary**: Map recurring character names, honorifics, or game terms in `~/.config/waytrans/glossary.json`.
- **System Tray Toggle**: Pause/resume translation or toggle `In-Place` / `Above` directly from your panel tray.
- **Cross-Platform Workspace**: Core OCR/layout/translation engine (`waytrans-core`, `waytrans-ocr`) is separated from platform capture/overlay crates (`waytrans-wayland`, plus scaffolded `waytrans-windows`).

---

## Workspace Structure

```text
crates/
├── waytrans-core     # Frame diffing, preprocessing, layout/collision, tracking, translation cache & APIs
├── waytrans-ocr      # Persistent libtesseract FFI via libloading, TSV parser, model downloader
├── waytrans-wayland  # grim/slurp capture, zwlr_layer_shell_v1 + tiny-skia + cosmic-text overlay
├── waytrans-windows  # Windows capture/overlay backend scaffold
└── waytrans-cli      # CLI entrypoint, single-instance live HUD loop, SNI system tray icon
```

---

## Requirements & Dependencies

### Arch Linux / CachyOS / Manjaro
```bash
sudo pacman -S rust grim slurp tesseract wl-clipboard libnotify
```

### Fedora / RHEL
```bash
sudo dnf install rust cargo grim slurp tesseract wl-clipboard libnotify
```

### Ubuntu / Debian
```bash
sudo apt install cargo rustc grim slurp tesseract-ocr libtesseract-dev wl-clipboard libnotify-bin
```

*(Note: `tessdata_fast` / `tessdata_best` Japanese models are required; `waytrans --setup-models` downloads them automatically.)*

---

## Installation

### Option 1: Quick Install (Recommended)
```bash
git clone https://github.com/remsysc/waytrans.git
cd waytrans
./install.sh
```
This builds the optimized release binary via `cargo build --release`, installs it to `~/.local/bin/waytrans`, and optionally downloads Japanese OCR models.

### Option 2: Cargo Install
```bash
cargo install --path crates/waytrans-cli
```

---

## Setting Up Japanese Models

```bash
waytrans --setup-models
```
Downloads `jpn.traineddata` and `jpn_vert.traineddata` into `~/.local/share/tessdata/` (and/or `~/.local/share/tessdata_fast/`).

---

## Translation Engines & Glossary

By default `waytrans` uses `--engine google` (fast, zero configuration). You can also use a local Ollama model or OpenAI/Gemini API:

```bash
# Local Ollama (recommended for context-aware VN dialogue)
waytrans -l --style inplace \
  --engine openai \
  --api-url http://127.0.0.1:11434/v1/chat/completions \
  --model qwen2.5:3b

# Gemini API (set GEMINI_API_KEY in environment)
waytrans -l --engine gemini --model gemini-2.5-flash
```

Optional glossary file (`~/.config/waytrans/glossary.json`):
```json
{
  "花村陽介": "Yosuke Hanamura",
  "マヨナカテレビ": "the Midnight Channel"
}
```

---

## Compositor Keybindings

### Hyprland
In `~/.config/hypr/hyprland.conf`:
```ini
# One-shot region translation (Super + Alt + J)
bind = SUPER_ALT, J, exec, waytrans -s ja -t en

# Game HUD Live Mode toggle (Super + Alt + L)
bind = SUPER_ALT, L, exec, waytrans --live -s ja -t en
```

Or in `~/.config/hypr/custom/keybinds.lua`:
```lua
hl.bind("SUPER + ALT + J", hl.dsp.exec_cmd(HOME .. "/.local/bin/waytrans -s ja -t en"))
hl.bind("SUPER + ALT + L", hl.dsp.exec_cmd(HOME .. "/.local/bin/waytrans --live -s ja -t en"))
```

### Sway / River
```ini
# Sway
bindsym $mod+Mod1+j exec waytrans -s ja -t en
bindsym $mod+Mod1+l exec waytrans --live -s ja -t en
```
```bash
# River
riverctl map normal Super+Alt J spawn "waytrans -s ja -t en"
riverctl map normal Super+Alt L spawn "waytrans --live -s ja -t en"
```

---

## CLI Usage & Flags

```text
Usage: waytrans [OPTIONS]

Options:
  -s, --source <SOURCE>      Source language for OCR and translation [default: ja]
  -t, --target <TARGET>      Target language for translation [default: en]
      --timeout <TIMEOUT>    Overlay duration in seconds before auto-dismiss [default: 10]
      --psm <PSM>            Tesseract Page Segmentation Mode (0-13) [default: 6]
      --no-overlay           Disable in-place floating overlay (desktop notification only)
      --no-clipboard         Disable copying translation to clipboard
      --notify               Send a desktop notification in addition to the on-screen overlay
  -l, --live                 Toggle dynamic live translation mode on or off
      --interval <INTERVAL>  Polling interval in seconds for live translation [default: 0.15]
      --fullscreen           Enable full-screen HUD mode without region selection
      --region               Use interactive region selection instead of fullscreen mode
      --zone <ZONE>          Screen scan zone in live mode [default: all] [possible: all, bottom]
      --style <STYLE>        Live overlay placement style [default: inplace] [possible: above, inplace]
      --engine <ENGINE>      Translation engine [default: google] [possible: google, openai, gemini]
      --api-url <API_URL>    OpenAI-compatible /v1/chat/completions URL (env: WAYTRANS_API_URL)
      --model <MODEL>        Model name for --engine openai or gemini (env: WAYTRANS_MODEL)
      --no-tray              Disable system tray icon in live mode
      --stop-live            Stop any currently running live translation session
      --setup-models         Download high-accuracy Japanese OCR models
  -V, --verbose              Enable verbose debug logging
  -v, --version              Print version
  -h, --help                 Print help
```

---

## License

Distributed under the [MIT License](LICENSE).
