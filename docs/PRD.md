# Product Requirements Document (PRD): Seamless In-Game Live Translation ("Game HUD Mode")

- **Status**: Draft
- **Target Component**: `waytrans`
- **Owner**: Rem (@remsysc)
- **Document Version**: 1.0.0

---

## 1. Executive Summary & Problem Statement

### 1.1 The Problem
Existing Linux screen translation tools (including standard notification-based scripts and generic windowed overlays) break immersion during gameplay, visual novel reading, and media viewing:
1. **Intrusive Window Containers**: Displaying translations inside opaque rectangles, rounded dialog boxes, or window borders obstructs game artwork, character sprites, and UI elements.
2. **Manual Region Selection Friction**: Requiring users to drag a selection box (`slurp`) every time they start a game or toggle translation is tedious and breaks fullscreen flow.
3. **Foreign Aesthetic**: Translations look like third-party software plastered over the game rather than an authentic, built-in localized subtitle track.

### 1.2 The Vision
A **seamless, containerless, fullscreen HUD translator** for Wayland. When toggled on, translated English subtitles render directly on top of Japanese game text with game-native typography (clean drop shadows/text strokes, zero opaque container boxes). To any outside observer, the subtitles look like an official, native feature of the game.

---

## 2. Target Users & Use Cases

- **Japanese Game & Visual Novel Players**: Playing untranslated JRPGs, visual novels, or Steam games on Linux/Wayland who want real-time English subtitles without Alt-Tabbing or reading notification popups.
- **Manga & Digital Media Readers**: Browsing full-screen raw Japanese manga or web content with instant, inline English translations.
- **Language Learners**: Immersing in native Japanese audio and text while having real-time English literal translation floating in-place as a reading aid.

---

## 3. Goals & Non-Goals

### 3.1 Goals
- **G-1 (Containerless Aesthetics)**: Eliminate all cards, boxes, borders, and dark rectangular backdrops. Only the translated glyphs with readable shadows/outlines are rendered.
- **G-2 (Zero-Friction Fullscreen Toggle)**: Toggle translation on/off with a single hotkey across the entire active screen or focused game window without requiring interactive drag selection.
- **G-3 (Native-Looking In-Place Placement)**: Position translated dialogue precisely relative to the source Japanese text, aligning with the dialogue rhythm.
- **G-4 (Zero Input Interference)**: 100% click-through and keypress pass-through so game controls, clicks, and movement are never intercepted.
- **G-5 (Real-Time Performance)**: Sub-second response time from the moment new Japanese dialogue appears to when the translation displays.

### 3.2 Non-Goals
- Game memory hooking / process injection (this remains a purely non-invasive Wayland compositor screen layer tool).
- Machine-learning voice-to-text / speech recognition (visual screen OCR only).
- Replacing standard one-shot snip mode (`waytrans` will retain area snip mode for general desktop use).

---

## 4. Requirements

| ID | Title | Priority | Description & Acceptance Criteria |
| :--- | :--- | :---: | :--- |
| **REQ-1** | **One-Touch Fullscreen Toggle** | **P0** | When toggling live mode (`waytrans --live --fullscreen`), the application immediately captures and monitors the active monitor without invoking `slurp`. Toggling the shortcut again immediately stops monitoring and clears overlays. |
| **REQ-2** | **Containerless HUD Typography** | **P0** | Subtitles MUST NOT render inside a visible card, box, or opaque rectangle. The window layer MUST be 100% transparent. Subtitle text MUST use high-legibility styling (white text with dark text stroke/glow or CSS `text-shadow`) so it is readable against both pure black and pure white game backgrounds without a background panel. |
| **REQ-3** | **In-Place Subtitle Anchoring** | **P0** | Translations must appear directly in-place: floating immediately above or directly over the recognized dialogue line, scaling proportionally with the original font size. |
| **REQ-4** | **100% Click-Through Pass-Through** | **P0** | The overlay surface MUST use `cairo.Region()` input mask combination or Wayland empty input region so mouse clicks, drags, and mouse wheel events pass cleanly to the game beneath. |
| **REQ-5** | **Dynamic Scene & Text Diffing** | **P1** | The watcher compares consecutive frames using perceptual hashing. If the game dialogue has not changed, zero OCR and zero translation calls are executed (0% idle CPU). When new dialogue appears, it detects the change and updates the subtitle within 500ms. |
| **REQ-6** | **Automatic Subtitle Fade / Clear** | **P1** | When dialogue disappears from the screen (e.g. cutscene transition, combat start, menu opening), the floating subtitle text immediately disappears, leaving the game display completely clear. |
| **REQ-7** | **In-Memory Translation Cache** | **P1** | Re-encountered dialogue strings, recurring UI options, or repeated phrases must return from an in-memory LRU cache in `0.00ms` with zero network requests. |
| **REQ-8** | **Configurable Styling (Fonts & Colors)** | **P2** | Allow users to customize subtitle font family (e.g. `Maple Mono NF CN`, `Noto Sans CJK JP`), font size, outline color, and shadow intensity via a configuration file or CLI flags. |

---

## 5. User Interaction & Workflow

```text
[ Gaming Session Active (e.g. Steam / Wine / Native Game) ]
                            │
                            ▼
           User presses shortcut: Super + Alt + L
                            │
                            ▼
              [ Wayland HUD Overlay Activates ]
   • Automatically identifies focused monitor
   • 100% transparent canvas initialized on OVERLAY layer
   • Zero mouse/keyboard capture (game input unhindered)
                            │
                            ▼
             [ Continuous Non-Invasive Loop ]
   1. Fast frame capture of game display
   2. Hash comparison:
      ├─ No change ──► Sleep (0% CPU)
      └─ New text  ──► OCR + Batch Translate
                            │
                            ▼
         [ Seamless In-Game Subtitles Displayed ]
   • White text with subtle dark halo/shadow
   • Floating directly relative to Japanese dialogue
   • No container cards or borders visible
                            │
                            ▼
           User presses shortcut: Super + Alt + L
                            │
                            ▼
    [ Overlay Dismisses Immediately — Clean Exit ]
```

---

## 6. Success Criteria & Observable Behavior

1. **Visual Immersion**: In screen recordings or screenshots of gameplay, an observer cannot distinguish between a built-in game subtitle and the `waytrans` overlay.
2. **Gameplay Fluidity**: Fast-clicking to advance visual novel dialogue suffers zero input delay or dropped clicks.
3. **No Screen Clutter**: When characters are not speaking, zero UI elements, placeholders, or badges exist on screen.
4. **Reliability**: Running continuously for 1+ hours produces zero memory leaks, process crashes, or runaway CPU usage.

---

## 7. Open Questions & Technical Considerations

1. **Focused Window vs Full Monitor**:
   - *Option A*: Always monitor the entire active monitor (`Quickshell.screens` / `hyprctl activeworkspace`).
   - *Option B*: Auto-detect the focused game window via `hyprctl activewindow` and restrict OCR to that window geometry (saves OCR cycles).
   - *Provisional Recommendation*: Support `--fullscreen` (whole monitor) by default, with `--active-window` as an optional performance flag.

2. **OCR Sub-Region vs Fullscreen Scanning**:
   - Scanning a 4K or 1080p full frame with Tesseract every 500ms can take ~300ms.
   - *Optimization*: Games usually place dialogue in the bottom 30% of the screen. We can provide an optional preset `--dialogue-zone bottom` (scans only `y: 70% -> 100%`) for maximum FPS during gameplay.
