use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WaytransError {
    #[error("capture error: {0}")]
    Capture(String),
    #[error("ocr error: {0}")]
    Ocr(String),
    #[error("translation error: {0}")]
    Translation(String),
    #[error("overlay error: {0}")]
    Overlay(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, WaytransError>;

/// Captured RGB24 frame in logical scan coordinates.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Packed RGB24 bytes (`width * height * 3`).
    pub rgb: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, rgb: Vec<u8>) -> Self {
        Self { width, height, rgb }
    }
}

/// Axis-aligned rectangle in pixels (x, y, right, bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w.max(1),
            bottom: y + h.max(1),
        }
    }

    pub fn width(&self) -> i32 {
        (self.right - self.left).max(1)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(1)
    }

    pub fn area(&self) -> i64 {
        (self.width() as i64) * (self.height() as i64)
    }
}

/// Return true if two bounding boxes overlap (with optional padding margin).
pub fn boxes_overlap(a: Rect, b: Rect, margin: i32) -> bool {
    !(a.right + margin < b.left
        || b.right + margin < a.left
        || a.bottom + margin < b.top
        || b.bottom + margin < a.top)
}

/// Return true if intersection covers at least `min_fraction` of the smaller box.
pub fn significant_overlap(a: Rect, b: Rect, min_fraction: f64) -> bool {
    let iw = a.right.min(b.right) - a.left.max(b.left);
    let ih = a.bottom.min(b.bottom) - a.top.max(b.top);
    if iw <= 0 || ih <= 0 {
        return false;
    }
    let smaller = a.area().min(b.area());
    smaller > 0 && ((iw as f64) * (ih as f64)) / (smaller as f64) >= min_fraction
}

/// Single OCR line with bounding box in scan-logical coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub conf: f32,
}

impl OcrLine {
    pub fn right(&self) -> i32 {
        self.left + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.top + self.height
    }
}

/// Unified dialogue or menu block formed by clustering OCR lines.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogueBlock {
    pub lines: Vec<OcrLine>,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub combined_text: String,
    pub avg_line_height: i32,
}

impl DialogueBlock {
    pub fn rect(&self) -> Rect {
        Rect {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    pub fn width(&self) -> i32 {
        (self.right - self.left).max(1)
    }

    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayStyle {
    Inplace,
    Above,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Google,
    OpenAi,
    Gemini,
}

impl Engine {
    pub fn as_str(&self) -> &'static str {
        match self {
            Engine::Google => "google",
            Engine::OpenAi => "openai",
            Engine::Gemini => "gemini",
        }
    }
}

/// Render item sent to platform `OverlayRenderer`.
#[derive(Debug, Clone)]
pub struct OverlayItem {
    pub pos_x: i32,
    pub pos_y: i32,
    pub font_size: i32,
    pub max_width: i32,
    pub est_height: i32,
    pub actual_w: i32,
    pub trans: String,
    pub src_left_mon: i32,
    pub src_w: i32,
    pub src_top_mon: i32,
    pub src_bottom_mon: i32,
}

/// Structured rolling turn for context-aware LLM translation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DialogueTurn {
    pub speaker: String,
    pub source: String,
    pub translation: String,
}

/// Cross-platform screen capture backend.
pub trait ScreenCapture: Send {
    fn capture(&mut self) -> Result<Frame>;
    fn scan_geometry(&self) -> (i32, i32, u32, u32);
    fn monitor_geometry(&self) -> (i32, i32, u32, u32);
    fn captures_own_overlay(&self) -> bool {
        true
    }
}

/// Cross-platform click-through subtitle overlay renderer.
pub trait OverlayRenderer: Send {
    /// Measure/resolve collisions and render `items` on screen.
    /// Returns rendered subtitle bounding rectangles in scan-local coordinates.
    fn update_subtitles(
        &mut self,
        items: &mut [OverlayItem],
        source_obstacles: &[Rect],
        style: OverlayStyle,
        scan_offset: (i32, i32),
    ) -> Result<Vec<Rect>>;

    fn set_visible(&mut self, visible: bool) -> Result<()>;
    fn shutdown(&mut self) {}
}

/// Cross-platform OCR backend operating on binarized grayscale buffers.
pub trait OcrEngine: Send + Sync {
    fn read_lines(
        &self,
        gray_pixels: &[u8],
        width: u32,
        height: u32,
        lang: &str,
        psm: i32,
        min_confidence: f32,
        min_line_confidence: f32,
        coord_scale: f64,
    ) -> Result<Vec<OcrLine>>;
}
