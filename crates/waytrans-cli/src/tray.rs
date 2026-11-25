use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use waytrans_core::OverlayStyle;

#[derive(Clone)]
pub struct TrayState {
    pub enabled: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
    pub style: Arc<Mutex<OverlayStyle>>,
}

#[cfg(target_os = "linux")]
pub struct TrayGuard {
    handle: ksni::Handle<WaytransTray>,
}

#[cfg(target_os = "linux")]
impl TrayGuard {
    pub fn refresh(&self) {
        self.handle.update(|_| {});
    }
}

#[cfg(target_os = "linux")]
impl Drop for TrayGuard {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

#[cfg(not(target_os = "linux"))]
pub struct TrayGuard;

#[cfg(not(target_os = "linux"))]
impl TrayGuard {
    pub fn refresh(&self) {}
}

#[cfg(target_os = "linux")]
struct WaytransTray {
    state: TrayState,
}

#[cfg(target_os = "linux")]
fn build_badge_icon(active: bool) -> Vec<ksni::Icon> {
    let w = 24i32;
    let h = 24i32;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    let (bg_r, bg_g, bg_b) = if active {
        (64u8, 192u8, 128u8) // Green when active
    } else {
        (110u8, 114u8, 128u8) // Muted gray when paused
    };
    let cx = 11.5_f32;
    let cy = 11.5_f32;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            // Draw a crisp circular badge with a small white "文" / glyph bar inside
            let in_circle = dist <= 10.2;
            let in_glyph = (7..=16).contains(&x) && (y == 8 || y == 11)
                || (x == 11 || x == 12) && (6..=17).contains(&y)
                || (x == 9 && (12..=15).contains(&y))
                || (x == 14 && (12..=15).contains(&y));
            let (a, r, g, b) = if !in_circle {
                (0u8, 0u8, 0u8, 0u8)
            } else if in_glyph {
                (255u8, 255u8, 255u8, 255u8)
            } else if dist >= 8.8 {
                (255u8, 20u8, 24u8, 32u8)
            } else {
                (255u8, bg_r, bg_g, bg_b)
            };
            // ksni::Icon expects network-order ARGB32 bytes
            data.extend_from_slice(&[a, r, g, b]);
        }
    }
    vec![ksni::Icon {
        width: w,
        height: h,
        data,
    }]
}

#[cfg(target_os = "linux")]
impl ksni::Tray for WaytransTray {
    fn id(&self) -> String {
        "waytrans".to_string()
    }

    fn title(&self) -> String {
        if self.state.enabled.load(Ordering::Relaxed) {
            "waytrans (Active)".to_string()
        } else {
            "waytrans (Paused)".to_string()
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        build_badge_icon(self.state.enabled.load(Ordering::Relaxed))
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let active = self.state.enabled.load(Ordering::Relaxed);
        ksni::ToolTip {
            title: if active {
                "waytrans — Live Translation Active".to_string()
            } else {
                "waytrans — Paused (Click to Resume)".to_string()
            },
            description: "Left-click to pause/resume live translation".to_string(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let prev = self.state.enabled.load(Ordering::Relaxed);
        self.state.enabled.store(!prev, Ordering::Relaxed);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let active = self.state.enabled.load(Ordering::Relaxed);
        let current_style = *self.state.style.lock().unwrap();
        let style_label = match current_style {
            OverlayStyle::Inplace => "Style: In-Place (Switch to Above)",
            OverlayStyle::Above => "Style: Above (Switch to In-Place)",
        };
        vec![
            StandardItem {
                label: if active {
                    "Pause Live Translation".to_string()
                } else {
                    "Enable Live Translation".to_string()
                },
                activate: Box::new(|this: &mut Self| {
                    let prev = this.state.enabled.load(Ordering::Relaxed);
                    this.state.enabled.store(!prev, Ordering::Relaxed);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: style_label.to_string(),
                activate: Box::new(|this: &mut Self| {
                    if let Ok(mut s) = this.state.style.lock() {
                        *s = match *s {
                            OverlayStyle::Inplace => OverlayStyle::Above,
                            OverlayStyle::Above => OverlayStyle::Inplace,
                        };
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit waytrans".to_string(),
                activate: Box::new(|this: &mut Self| {
                    this.state.running.store(false, Ordering::SeqCst);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
pub fn start_tray(state: TrayState) -> Option<TrayGuard> {
    let service = ksni::TrayService::new(WaytransTray { state });
    let handle = service.handle();
    service.spawn();
    Some(TrayGuard { handle })
}

#[cfg(not(target_os = "linux"))]
pub fn start_tray(_state: TrayState) -> Option<TrayGuard> {
    None
}
