use waytrans_core::{
    Frame, OverlayItem, OverlayRenderer, OverlayStyle, Rect, Result, ScreenCapture, WaytransError,
};

/// Windows screen/window capture backend (Windows.Graphics.Capture / DXGI).
pub struct WindowsCapture {
    scan_x: i32,
    scan_y: i32,
    scan_w: u32,
    scan_h: u32,
}

impl WindowsCapture {
    pub fn new(scan_x: i32, scan_y: i32, scan_w: u32, scan_h: u32) -> Self {
        Self {
            scan_x,
            scan_y,
            scan_w,
            scan_h,
        }
    }
}

impl ScreenCapture for WindowsCapture {
    fn capture(&mut self) -> Result<Frame> {
        Err(WaytransError::Capture(
            "WindowsCapture requires running on Windows (Windows.Graphics.Capture)".into(),
        ))
    }

    fn scan_geometry(&self) -> (i32, i32, u32, u32) {
        (self.scan_x, self.scan_y, self.scan_w, self.scan_h)
    }

    fn monitor_geometry(&self) -> (i32, i32, u32, u32) {
        (self.scan_x, self.scan_y, self.scan_w, self.scan_h)
    }

    fn captures_own_overlay(&self) -> bool {
        // Windows WDA_EXCLUDEFROMCAPTURE excludes the overlay window natively.
        false
    }
}

/// Windows click-through layered HUD overlay (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST).
pub struct WindowsOverlay;

impl WindowsOverlay {
    pub fn new() -> Result<Self> {
        Err(WaytransError::Overlay(
            "WindowsOverlay requires running on Windows".into(),
        ))
    }
}

impl OverlayRenderer for WindowsOverlay {
    fn update_subtitles(
        &mut self,
        _items: &mut [OverlayItem],
        _source_obstacles: &[Rect],
        _style: OverlayStyle,
        _scan_offset: (i32, i32),
    ) -> Result<Vec<Rect>> {
        Err(WaytransError::Overlay(
            "WindowsOverlay requires running on Windows".into(),
        ))
    }

    fn set_visible(&mut self, _visible: bool) -> Result<()> {
        Ok(())
    }
}
