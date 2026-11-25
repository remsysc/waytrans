use crate::types::{Frame, Rect};

#[derive(Debug, Clone)]
pub struct PreprocessedImage {
    pub width: u32,
    pub height: u32,
    /// Binarized grayscale image: 0 = foreground stroke, 255 = white background.
    pub pixels: Vec<u8>,
    pub coord_scale: f64,
}

fn erase_rect(buf: &mut [u8], w: usize, h: usize, rect: Rect, scale: f64, fill: u8) {
    let x1 = ((rect.left as f64 * scale) as i32 - 2).clamp(0, w as i32) as usize;
    let y1 = ((rect.top as f64 * scale) as i32 - 2).clamp(0, h as i32) as usize;
    let x2 = ((rect.right as f64 * scale) as i32 + 2).clamp(0, w as i32) as usize;
    let y2 = ((rect.bottom as f64 * scale) as i32 + 2).clamp(0, h as i32) as usize;
    for y in y1..y2 {
        let row = &mut buf[y * w..y * w + w];
        row[x1..x2].fill(fill);
    }
}

fn box_blur_u8(src: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let stride = w + 1;
    let mut integral = vec![0u64; (h + 1) * stride];
    for y in 0..h {
        let mut row_sum = 0u64;
        let src_row = &src[y * w..(y + 1) * w];
        let prev_row = y * stride;
        let curr_row = (y + 1) * stride;
        for x in 0..w {
            row_sum += src_row[x] as u64;
            integral[curr_row + x + 1] = integral[prev_row + x + 1] + row_sum;
        }
    }

    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(h);
        let row_out = &mut out[y * w..(y + 1) * w];
        for x in 0..w {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(w);
            let area = ((y1 - y0) * (x1 - x0)) as u64;
            let sum = integral[y1 * stride + x1] + integral[y0 * stride + x0]
                - integral[y0 * stride + x1]
                - integral[y1 * stride + x0];
            row_out[x] = (sum / area.max(1)) as u8;
        }
    }
    out
}

fn min_filter_3x3(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![255u8; w * h];
    for y in 0..h {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        for x in 0..w {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let mut m = 255u8;
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    let v = src[ny * w + nx];
                    if v < m {
                        m = v;
                    }
                }
            }
            out[y * w + x] = m;
        }
    }
    out
}

fn max_filter_3x3(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        for x in 0..w {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let mut m = 0u8;
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    let v = src[ny * w + nx];
                    if v > m {
                        m = v;
                    }
                }
            }
            out[y * w + x] = m;
        }
    }
    out
}

fn resize_gray_bilinear(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return vec![0; dw * dh];
    }
    let mut out = vec![0u8; dw * dh];
    let scale_x = sw as f64 / dw as f64;
    let scale_y = sh as f64 / dh as f64;
    for dy in 0..dh {
        let fy = ((dy as f64 + 0.5) * scale_y - 0.5).clamp(0.0, (sh - 1) as f64);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = fy - y0 as f64;
        for dx in 0..dw {
            let fx = ((dx as f64 + 0.5) * scale_x - 0.5).clamp(0.0, (sw - 1) as f64);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let wx = fx - x0 as f64;

            let p00 = src[y0 * sw + x0] as f64;
            let p10 = src[y0 * sw + x1] as f64;
            let p01 = src[y1 * sw + x0] as f64;
            let p11 = src[y1 * sw + x1] as f64;
            let top = p00 * (1.0 - wx) + p10 * wx;
            let bot = p01 * (1.0 - wx) + p11 * wx;
            out[dy * dw + dx] = (top * (1.0 - wy) + bot * wy).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn find_horizontal_runs(
    grid: &[u8],
    gw: usize,
    gh: usize,
    pixel_threshold: u8,
    min_cols: usize,
    max_gap: usize,
) -> Vec<(usize, usize, usize)> {
    let mut runs = Vec::new();
    for gy in 0..gh {
        let row = &grid[gy * gw..(gy + 1) * gw];
        let bright_total = row.iter().filter(|&&v| v > pixel_threshold).count();
        if bright_total < min_cols {
            continue;
        }
        let mut best_start = 0usize;
        let mut best_end: isize = -1;
        let mut cur_start: Option<usize> = None;
        let mut gap = 0usize;

        for (gx, &val) in row.iter().enumerate() {
            if val > pixel_threshold {
                if cur_start.is_none() {
                    cur_start = Some(gx);
                }
                let cur_end = gx as isize;
                gap = 0;
                if let Some(cs) = cur_start {
                    if (cur_end - cs as isize) > (best_end - best_start as isize) {
                        best_start = cs;
                        best_end = cur_end;
                    }
                }
            } else if cur_start.is_some() {
                gap += 1;
                if gap > max_gap {
                    cur_start = None;
                    gap = 0;
                }
            }
        }
        if best_end >= best_start as isize {
            let be = best_end as usize;
            let bright_cols = (best_start..=be).filter(|&gx| row[gx] > pixel_threshold).count();
            if bright_cols >= min_cols {
                runs.push((gy, best_start, be));
            }
        }
    }
    runs
}

/// Binarize game/UI text (both bright-on-dark and dark-on-light) while erasing active
/// subtitle rectangles (`mask_rects`) into pure white (255).
pub fn preprocess_frame(frame: &Frame, mask_rects: &[Rect]) -> PreprocessedImage {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 || frame.rgb.len() < w * h * 3 {
        return PreprocessedImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            coord_scale: 1.0,
        };
    }

    let (coord_scale, w_s, h_s) = if w > 500 && h > 90 {
        let scale = 0.75_f64.min(1440.0 / (w.max(h) as f64));
        let sw = ((w as f64 * scale) as usize).max(32);
        let sh = ((h as f64 * scale) as usize).max(20);
        (scale, sw, sh)
    } else {
        (1.0, w, h)
    };

    let n = w_s * h_s;
    let mut gray = vec![0u8; n];
    let mut max_rgb = vec![0u8; n];
    let mut orange_mask = vec![0u8; n];
    let mut has_orange = false;

    for dy in 0..h_s {
        let sy = if w_s == w && h_s == h {
            dy
        } else {
            (((dy as f64 + 0.5) * h as f64) / h_s as f64).floor().min((h - 1) as f64) as usize
        };
        for dx in 0..w_s {
            let sx = if w_s == w && h_s == h {
                dx
            } else {
                (((dx as f64 + 0.5) * w as f64) / w_s as f64).floor().min((w - 1) as f64) as usize
            };
            let sidx = (sy * w + sx) * 3;
            let r = frame.rgb[sidx];
            let g = frame.rgb[sidx + 1];
            let b = frame.rgb[sidx + 2];
            let di = dy * w_s + dx;
            gray[di] = ((299u32 * r as u32 + 587u32 * g as u32 + 114u32 * b as u32 + 500) / 1000) as u8;
            max_rgb[di] = r.max(g).max(b);
            if r > 200 && g > 105 && g < 185 && b < 60 {
                orange_mask[di] = 255;
                has_orange = true;
            }
        }
    }

    let mut raw_gray = gray.clone();
    let gw = (w_s / 16).max(1);
    let gh = (h_s / 4).max(1);

    let min_gray = min_filter_3x3(&gray, w_s, h_s);
    let pill_grid = resize_gray_bilinear(&min_gray, w_s, h_s, gw, gh);
    let pill_rows = find_horizontal_runs(
        &pill_grid,
        gw,
        gh,
        225,
        ((gw as f64 * 0.22) as usize).max(14),
        4,
    );

    let orange_rows = if has_orange {
        let mid_w = (w_s / 4).max(gw);
        let mid_h = (h_s / 2).max(gh);
        let og_mid = resize_gray_bilinear(&orange_mask, w_s, h_s, mid_w, mid_h);
        let og_max = max_filter_3x3(&og_mid, mid_w, mid_h);
        let og_grid = resize_gray_bilinear(&og_max, mid_w, mid_h, gw, gh);
        find_horizontal_runs(
            &og_grid,
            gw,
            gh,
            220,
            ((gw as f64 * 0.15) as usize).max(10),
            2,
        )
    } else {
        Vec::new()
    };

    for &rect in mask_rects {
        erase_rect(&mut gray, w_s, h_s, rect, coord_scale, 90);
        erase_rect(&mut max_rgb, w_s, h_s, rect, coord_scale, 90);
        erase_rect(&mut raw_gray, w_s, h_s, rect, coord_scale, 255);
    }

    let local_bg = box_blur_u8(&gray, w_s, h_s, 24);
    let local_max_bg = box_blur_u8(&max_rgb, w_s, h_s, 24);

    let mut bw = vec![255u8; n];
    for i in 0..n {
        let light_outside = (max_rgb[i] as i16 - local_max_bg[i] as i16) > 24 && max_rgb[i] > 110;
        let dark_inside =
            (local_bg[i] as i16 - gray[i] as i16) > 28 && local_bg[i] > 160 && gray[i] < 145;
        if light_outside || dark_inside {
            bw[i] = 0;
        }
    }

    if !pill_rows.is_empty() {
        let min_gy = pill_rows.iter().map(|r| r.0).min().unwrap_or(0);
        let max_gy = pill_rows.iter().map(|r| r.0).max().unwrap_or(0);
        let min_gx = pill_rows.iter().map(|r| r.1).min().unwrap_or(0);
        let mut max_gxs: Vec<usize> = pill_rows.iter().map(|r| r.2).collect();
        max_gxs.sort_unstable();
        let max_gx = max_gxs[max_gxs.len() / 2];

        let px1 = (min_gx * 16 + 4).min(w_s);
        let py1 = (min_gy * 4 + 6).min(h_s);
        let px2 = ((max_gx + 1) * 16).saturating_sub(4).min(w_s);
        let py2 = ((max_gy + 1) * 4).saturating_sub(6).min(h_s);
        if px2 > px1 && py2 > py1 {
            let cy1 = py1.saturating_sub(8);
            let cy2 = (py2 + 6).min(h_s);
            let cx1 = px1.saturating_sub(16);
            let cx2 = (px2 + 16).min(w_s);
            for y in cy1..cy2 {
                bw[y * w_s + cx1..y * w_s + cx2].fill(255);
            }
            for y in py1..py2 {
                for x in px1..px2 {
                    bw[y * w_s + x] = if raw_gray[y * w_s + x] < 165 { 0 } else { 255 };
                }
            }
        }
    }

    if !orange_rows.is_empty() {
        let min_gy = orange_rows.iter().map(|r| r.0).min().unwrap_or(0);
        let max_gy = orange_rows.iter().map(|r| r.0).max().unwrap_or(0);
        let min_gx = orange_rows.iter().map(|r| r.1).min().unwrap_or(0);
        let max_gx = orange_rows.iter().map(|r| r.2).max().unwrap_or(0);

        let cy1 = (min_gy * 4).saturating_sub(45);
        let cy2 = ((max_gy + 1) * 4 + 6).min(h_s);
        let cx1 = (min_gx * 16).saturating_sub(16);
        let cx2 = ((max_gx + 1) * 16 + 16).min(w_s);
        for y in cy1..cy2 {
            bw[y * w_s + cx1..y * w_s + cx2].fill(255);
        }

        let ox1 = (min_gx * 16 + 12).min(w_s);
        let oy1 = (min_gy * 4 + 8).min(h_s);
        let ox2 = (ox1 + ((w_s as f64 * 0.28) as usize).max(350)).min(w_s);
        let oy2 = ((max_gy + 1) * 4).saturating_sub(4).min(h_s);
        if ox2 > ox1 && oy2 > oy1 {
            for y in oy1..oy2 {
                for x in ox1..ox2 {
                    bw[y * w_s + x] = if raw_gray[y * w_s + x] < 120 { 0 } else { 255 };
                }
            }
        }
    }

    for &rect in mask_rects {
        erase_rect(&mut bw, w_s, h_s, rect, coord_scale, 255);
    }

    PreprocessedImage {
        width: w_s as u32,
        height: h_s as u32,
        pixels: bw,
        coord_scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_rect(f: &mut Frame, x1: usize, y1: usize, x2: usize, y2: usize, rgb: [u8; 3]) {
        for y in y1..y2.min(f.height as usize) {
            for x in x1..x2.min(f.width as usize) {
                let idx = (y * f.width as usize + x) * 3;
                f.rgb[idx] = rgb[0];
                f.rgb[idx + 1] = rgb[1];
                f.rgb[idx + 2] = rgb[2];
            }
        }
    }

    #[test]
    fn preserves_colored_and_dark_on_light_text_and_erases_masks() {
        let mut f = Frame::new(1280, 720, vec![0u8; 1280 * 720 * 3]);
        fill_rect(&mut f, 0, 0, 1280, 720, [25, 25, 30]);
        // Colored blue text bar on dark bg
        fill_rect(&mut f, 100, 120, 220, 145, [60, 110, 255]);
        // Light card with dark text strokes
        fill_rect(&mut f, 100, 250, 360, 340, [225, 225, 220]);
        fill_rect(&mut f, 130, 280, 280, 286, [30, 30, 35]);
        // Masked white subtitle
        fill_rect(&mut f, 100, 400, 350, 430, [255, 255, 255]);

        let out = preprocess_frame(&f, &[Rect::from_xywh(100, 400, 250, 30)]);
        assert!((out.coord_scale - 0.75).abs() < 1e-6);
        let px = |x: usize, y: usize| -> u8 {
            let sx = (x as f64 * out.coord_scale) as usize;
            let sy = (y as f64 * out.coord_scale) as usize;
            out.pixels[sy * out.width as usize + sx]
        };
        assert_eq!(px(150, 132), 0);
        assert_eq!(px(180, 283), 0);
        assert_eq!(px(200, 415), 255);
    }
}
