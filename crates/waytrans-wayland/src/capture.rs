use std::process::Command;

use serde::Deserialize;
use waytrans_core::{Frame, Result, ScreenCapture, WaytransError};

#[derive(Deserialize)]
struct HyprMonitor {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: Option<f64>,
    transform: Option<i32>,
    focused: Option<bool>,
}

pub fn check_capture_dependencies(need_slurp: bool) -> Result<()> {
    let mut missing = Vec::new();
    if !which_exists("grim") {
        missing.push("grim");
    }
    if need_slurp && !which_exists("slurp") {
        missing.push("slurp");
    }
    if !missing.is_empty() {
        return Err(WaytransError::Capture(format!(
            "Missing required Wayland capture tools: {}. Install them via your package manager.",
            missing.join(", ")
        )));
    }
    Ok(())
}

pub(crate) fn which_exists(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file())
        })
        .unwrap_or(false)
}

/// Detect the logical geometry `(x, y, w, h)` of the currently focused monitor.
pub fn get_active_monitor_geometry() -> (i32, i32, u32, u32) {
    if which_exists("hyprctl") {
        if let Ok(out) = Command::new("hyprctl").args(["monitors", "-j"]).output() {
            if out.status.success() {
                if let Ok(monitors) = serde_json::from_slice::<Vec<HyprMonitor>>(&out.stdout) {
                    if let Some(m) = monitors
                        .iter()
                        .find(|m| m.focused == Some(true))
                        .or_else(|| monitors.first())
                    {
                        let scale = m.scale.filter(|&s| s > 0.0).unwrap_or(1.0);
                        let mut raw_w = m.width;
                        let mut raw_h = m.height;
                        if matches!(m.transform.unwrap_or(0), 1 | 3 | 5 | 7) {
                            std::mem::swap(&mut raw_w, &mut raw_h);
                        }
                        let lw = ((raw_w as f64 / scale).round() as i32).max(1) as u32;
                        let lh = ((raw_h as f64 / scale).round() as i32).max(1) as u32;
                        return (m.x, m.y, lw, lh);
                    }
                }
            }
        }
    }
    (0, 0, 1920, 1080)
}

/// Prompt the user to select a region with `slurp`, returning `(x, y, w, h)` or `None` if cancelled.
pub fn select_region_slurp(border_color: &str, bg_color: &str) -> Result<Option<(i32, i32, u32, u32)>> {
    check_capture_dependencies(true)?;
    let out = Command::new("slurp")
        .args(["-d", "-c", border_color, "-b", bg_color, "-w", "2"])
        .output()
        .map_err(|e| WaytransError::Capture(format!("Failed to run slurp: {e}")))?;

    if !out.status.success() {
        return Ok(None);
    }
    let geom = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if geom.is_empty() {
        return Ok(None);
    }
    let Some((coords, dims)) = geom.split_once(' ') else {
        return Err(WaytransError::Capture(format!(
            "Unexpected slurp geometry: '{geom}'"
        )));
    };
    let Some((xs, ys)) = coords.split_once(',') else {
        return Err(WaytransError::Capture(format!(
            "Unexpected slurp coords: '{geom}'"
        )));
    };
    let Some((ws, hs)) = dims.split_once('x') else {
        return Err(WaytransError::Capture(format!(
            "Unexpected slurp dims: '{geom}'"
        )));
    };
    let x: i32 = xs.parse().map_err(|_| WaytransError::Capture("bad x".into()))?;
    let y: i32 = ys.parse().map_err(|_| WaytransError::Capture("bad y".into()))?;
    let w: u32 = ws.parse().map_err(|_| WaytransError::Capture("bad w".into()))?;
    let h: u32 = hs.parse().map_err(|_| WaytransError::Capture("bad h".into()))?;
    Ok(Some((x, y, w, h)))
}

/// Parse binary PPM (`P6`) bytes directly into a `Frame` with zero disk I/O.
pub fn parse_ppm_p6(bytes: &[u8]) -> Result<Frame> {
    if !bytes.starts_with(b"P6") {
        return Err(WaytransError::Capture("Invalid PPM header (expected P6)".into()));
    }
    let mut idx = 2usize;
    let mut tokens = Vec::with_capacity(3);
    while tokens.len() < 3 && idx < bytes.len() {
        // Skip whitespace and comments
        while idx < bytes.len() {
            let b = bytes[idx];
            if b == b'#' {
                while idx < bytes.len() && bytes[idx] != b'\n' {
                    idx += 1;
                }
            } else if b.is_ascii_whitespace() {
                idx += 1;
            } else {
                break;
            }
        }
        let start = idx;
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if start < idx {
            tokens.push(
                std::str::from_utf8(&bytes[start..idx])
                    .map_err(|_| WaytransError::Capture("Non-UTF8 PPM header".into()))?,
            );
        }
    }
    if tokens.len() != 3 {
        return Err(WaytransError::Capture("Incomplete PPM header".into()));
    }
    // Single whitespace byte separates maxval from binary pixel data
    if idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let width: u32 = tokens[0]
        .parse()
        .map_err(|_| WaytransError::Capture("Invalid PPM width".into()))?;
    let height: u32 = tokens[1]
        .parse()
        .map_err(|_| WaytransError::Capture("Invalid PPM height".into()))?;
    let expected = (width as usize) * (height as usize) * 3;
    if bytes.len() < idx + expected {
        return Err(WaytransError::Capture("Truncated PPM payload".into()));
    }
    Ok(Frame::new(width, height, bytes[idx..idx + expected].to_vec()))
}

/// Wayland `grim` screen capture outputting raw PPM straight to stdout (no temp files).
pub struct GrimCapture {
    scan_x: i32,
    scan_y: i32,
    scan_w: u32,
    scan_h: u32,
    mon_x: i32,
    mon_y: i32,
    mon_w: u32,
    mon_h: u32,
    scan_geometry: String,
}

impl GrimCapture {
    pub fn new(
        scan_x: i32,
        scan_y: i32,
        scan_w: u32,
        scan_h: u32,
        mon_x: i32,
        mon_y: i32,
        mon_w: u32,
        mon_h: u32,
    ) -> Self {
        let scan_geometry = format!("{scan_x},{scan_y} {scan_w}x{scan_h}");
        Self {
            scan_x,
            scan_y,
            scan_w,
            scan_h,
            mon_x,
            mon_y,
            mon_w,
            mon_h,
            scan_geometry,
        }
    }
}

impl ScreenCapture for GrimCapture {
    fn capture(&mut self) -> Result<Frame> {
        let out = Command::new("grim")
            .args(["-s", "1", "-t", "ppm", "-g", &self.scan_geometry, "-"])
            .output()
            .map_err(|e| WaytransError::Capture(format!("grim failed: {e}")))?;

        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(WaytransError::Capture(format!("grim exited with error: {err}")));
        }
        parse_ppm_p6(&out.stdout)
    }

    fn scan_geometry(&self) -> (i32, i32, u32, u32) {
        (self.scan_x, self.scan_y, self.scan_w, self.scan_h)
    }

    fn monitor_geometry(&self) -> (i32, i32, u32, u32) {
        (self.mon_x, self.mon_y, self.mon_w, self.mon_h)
    }

    fn captures_own_overlay(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ppm_p6_header_and_pixels() {
        let mut bytes = b"P6\n# comment\n2 1\n255\n".to_vec();
        bytes.extend_from_slice(&[10, 20, 30, 40, 50, 60]);
        let f = parse_ppm_p6(&bytes).unwrap();
        assert_eq!(f.width, 2);
        assert_eq!(f.height, 1);
        assert_eq!(f.rgb, vec![10, 20, 30, 40, 50, 60]);
    }
}
