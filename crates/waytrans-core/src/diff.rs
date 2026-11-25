use crate::types::{Frame, Rect};

pub const SIGNATURE_W: u32 = 160;
pub const SIGNATURE_H: u32 = 90;

#[inline]
fn rgb_to_luma(r: u8, g: u8, b: u8) -> u8 {
    ((299u32 * r as u32 + 587u32 * g as u32 + 114u32 * b as u32 + 500) / 1000) as u8
}

#[inline]
fn point_in_rects(x: i32, y: i32, rects: &[Rect], pad: i32) -> bool {
    for r in rects {
        if x >= r.left - pad && x < r.right + pad && y >= r.top - pad && y < r.bottom + pad {
            return true;
        }
    }
    false
}

/// Compute a 160x90 grayscale byte thumbnail while masking out active subtitle rects.
pub fn compute_image_signature(frame: &Frame, mask_rects: &[Rect]) -> Vec<u8> {
    let mut out = vec![0u8; (SIGNATURE_W * SIGNATURE_H) as usize];
    if frame.width == 0 || frame.height == 0 || frame.rgb.len() < (frame.width * frame.height * 3) as usize {
        return out;
    }
    let fw = frame.width as usize;
    let fh = frame.height as usize;

    for sy in 0..SIGNATURE_H {
        let fy = (((sy as f64 + 0.5) * fh as f64) / SIGNATURE_H as f64).floor() as usize;
        let fy = fy.min(fh - 1);
        for sx in 0..SIGNATURE_W {
            let fx = (((sx as f64 + 0.5) * fw as f64) / SIGNATURE_W as f64).floor() as usize;
            let fx = fx.min(fw - 1);
            let val = if point_in_rects(fx as i32, fy as i32, mask_rects, 0) {
                128
            } else {
                let idx = (fy * fw + fx) * 3;
                rgb_to_luma(frame.rgb[idx], frame.rgb[idx + 1], frame.rgb[idx + 2])
            };
            out[(sy * SIGNATURE_W + sx) as usize] = val;
        }
    }
    out
}

/// Compute a 16x16 grayscale signature of a source bounding box.
pub fn region_patch_signature(frame: &Frame, rect: Rect) -> Option<Vec<u8>> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let x1 = rect.left.clamp(0, frame.width as i32 - 1);
    let y1 = rect.top.clamp(0, frame.height as i32 - 1);
    let x2 = rect.right.clamp(x1 + 1, frame.width as i32);
    let y2 = rect.bottom.clamp(y1 + 1, frame.height as i32);

    let cw = (x2 - x1) as f64;
    let ch = (y2 - y1) as f64;
    let fw = frame.width as usize;
    let mut out = vec![0u8; 16 * 16];

    for sy in 0..16 {
        let fy = (y1 as f64 + ((sy as f64 + 0.5) * ch / 16.0)).floor() as usize;
        let fy = fy.min(frame.height as usize - 1);
        for sx in 0..16 {
            let fx = (x1 as f64 + ((sx as f64 + 0.5) * cw / 16.0)).floor() as usize;
            let fx = fx.min(fw - 1);
            let idx = (fy * fw + fx) * 3;
            out[sy * 16 + sx] = rgb_to_luma(frame.rgb[idx], frame.rgb[idx + 1], frame.rgb[idx + 2]);
        }
    }
    Some(out)
}

/// Compute a 48x24 signature of the border ring around `rect`, masking interior & active pills.
pub fn border_ring_signature(
    frame: &Frame,
    rect: Rect,
    pad: i32,
    mask_rects: &[Rect],
) -> Option<Vec<u8>> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }
    let x1 = (rect.left - pad).max(0);
    let y1 = (rect.top - pad).max(0);
    let x2 = (rect.right + pad).min(frame.width as i32);
    let y2 = (rect.bottom + pad).min(frame.height as i32);
    if x2 <= x1 || y2 <= y1 {
        return None;
    }

    let cw = (x2 - x1) as f64;
    let ch = (y2 - y1) as f64;
    let fw = frame.width as usize;
    let mut out = vec![0u8; 48 * 24];

    for sy in 0..24 {
        let fy = (y1 as f64 + ((sy as f64 + 0.5) * ch / 24.0)).floor() as i32;
        let fy_clamped = fy.clamp(0, frame.height as i32 - 1) as usize;
        for sx in 0..48 {
            let fx = (x1 as f64 + ((sx as f64 + 0.5) * cw / 48.0)).floor() as i32;
            let fx_clamped = fx.clamp(0, frame.width as i32 - 1) as usize;
            let inside_src = fx >= rect.left && fx < rect.right && fy >= rect.top && fy < rect.bottom;
            let inside_mask = point_in_rects(fx, fy, mask_rects, 2);
            let val = if inside_src || inside_mask {
                128
            } else {
                let idx = (fy_clamped * fw + fx_clamped) * 3;
                rgb_to_luma(frame.rgb[idx], frame.rgb[idx + 1], frame.rgb[idx + 2])
            };
            out[sy * 48 + sx] = val;
        }
    }
    Some(out)
}

/// Return true if pixels outside masked areas changed meaningfully.
pub fn has_significant_change(
    prev: &[u8],
    curr: &[u8],
    threshold: u32,
    cell_delta: u8,
    min_changed_cells: usize,
) -> bool {
    if prev.len() != curr.len() {
        return true;
    }
    let mut diff_sum: u32 = 0;
    let mut changed_cells: usize = 0;
    for (&a, &b) in prev.iter().zip(curr.iter()) {
        let d = a.abs_diff(b);
        diff_sum += d as u32;
        if d > cell_delta {
            changed_cells += 1;
            if changed_cells >= min_changed_cells {
                return true;
            }
        }
    }
    diff_sum > threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_small_localized_change_and_ignores_masked_area() {
        let mut base = Frame::new(1920, 1080, vec![30u8; 1920 * 1080 * 3]);
        let mut changed = base.clone();
        for y in 900..930 {
            for x in 800..940 {
                let idx = (y * 1920 + x) * 3;
                changed.rgb[idx] = 135;
                changed.rgb[idx + 1] = 135;
                changed.rgb[idx + 2] = 135;
            }
        }
        let s1 = compute_image_signature(&base, &[]);
        let s2 = compute_image_signature(&changed, &[]);
        assert!(has_significant_change(&s1, &s2, 450, 12, 8));

        // Mask out the changed rectangle
        let mask = vec![Rect::from_xywh(800, 900, 140, 30)];
        let m1 = compute_image_signature(&base, &mask);
        let m2 = compute_image_signature(&changed, &mask);
        assert!(!has_significant_change(&m1, &m2, 450, 12, 8));
        base.rgb[0] = 30;
    }
}
