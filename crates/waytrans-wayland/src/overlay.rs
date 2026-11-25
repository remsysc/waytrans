use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cosmic_text::{
    Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
    Wrap,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Rect as SkiaRect, Transform};
use waytrans_core::layout::{compute_inplace_wrap_width, resolve_subtitle_collisions};
use waytrans_core::{OverlayItem, OverlayRenderer, OverlayStyle, Rect, Result, WaytransError};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

enum OverlayCmd {
    Render {
        items: Vec<PreparedDraw>,
        ack: SyncSender<()>,
    },
    SetVisible {
        visible: bool,
        ack: SyncSender<()>,
    },
    Shutdown,
}

#[derive(Clone)]
struct PreparedDraw {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    font_size: f32,
    text: String,
    style: OverlayStyle,
}

struct WaylandApp {
    registry_state: RegistryState,
    output_state: OutputState,
    _compositor_state: CompositorState,
    shm: Shm,
    _layer_shell: LayerShell,
    layer: LayerSurface,
    pool: Option<SlotPool>,
    configured: bool,
    width: u32,
    height: u32,
    visible: bool,
    current_items: Vec<PreparedDraw>,
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl WaylandApp {
    fn draw_current(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        let w = self.width;
        let h = self.height;
        let stride = (w * 4) as i32;

        if self.pool.is_none() {
            if let Ok(pool) = SlotPool::new((w * h * 4) as usize, &self.shm) {
                self.pool = Some(pool);
            }
        }
        let Some(pool) = self.pool.as_mut() else {
            return;
        };

        let Ok((buffer, canvas)) =
            pool.create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };

        if !self.visible || self.current_items.is_empty() {
            canvas.fill(0);
        } else if let Some(mut pixmap) = Pixmap::new(w, h) {
            for item in &self.current_items {
                render_item_to_pixmap(
                    &mut pixmap,
                    &mut self.font_system,
                    &mut self.swash_cache,
                    item,
                );
            }
            let src = pixmap.data();
            let len = canvas.len().min(src.len());
            canvas[..len].copy_from_slice(&src[..len]);
            // Convert tiny-skia premultiplied RGBA -> Wayland Argb8888 (little-endian BGRA)
            for px in canvas[..len].chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        } else {
            canvas.fill(0);
        }

        let surf = self.layer.wl_surface();
        let _ = surf.damage_buffer(0, 0, w as i32, h as i32);
        let _ = buffer.attach_to(surf);
        surf.commit();
    }
}

fn fill_rounded_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    color: Color,
) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(color);
        paint.anti_alias = true;
        pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
}

fn render_text_glyphs(
    pixmap: &mut Pixmap,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    buf: &Buffer,
    origin_x: i32,
    origin_y: i32,
    color: TextColor,
) {
    buf.draw(font_system, swash_cache, color, |gx, gy, gw, gh, c| {
        let a = c.a();
        if a == 0 || gw == 0 || gh == 0 {
            return;
        }
        let px = origin_x + gx;
        let py = origin_y + gy;
        if px < 0 || py < 0 || px >= pixmap.width() as i32 || py >= pixmap.height() as i32 {
            return;
        }
        if let Some(rect) = SkiaRect::from_xywh(px as f32, py as f32, gw as f32, gh as f32) {
            let mut paint = Paint::default();
            paint.set_color_rgba8(c.r(), c.g(), c.b(), a);
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }
    });
}

fn measure_text(
    font_system: &mut FontSystem,
    text: &str,
    font_size: f32,
    max_width: Option<f32>,
) -> (Buffer, f32, f32) {
    let line_h = (font_size * 1.28).ceil();
    let mut buf = Buffer::new(font_system, Metrics::new(font_size, line_h));
    buf.set_wrap(font_system, if max_width.is_some() { Wrap::WordOrGlyph } else { Wrap::None });
    buf.set_size(font_system, max_width, None);
    let attrs = Attrs::new().family(Family::SansSerif).weight(Weight::BOLD);
    buf.set_text(font_system, text, attrs, Shaping::Advanced);
    buf.shape_until_scroll(font_system, false);

    let mut max_w = 0.0_f32;
    let mut total_h = 0.0_f32;
    let mut line_count = 0usize;
    for run in buf.layout_runs() {
        max_w = max_w.max(run.line_w);
        total_h = total_h.max(run.line_top + line_h);
        line_count += 1;
    }
    if line_count == 0 {
        total_h = line_h;
    }
    (buf, max_w.ceil(), total_h.ceil())
}

fn render_item_to_pixmap(
    pixmap: &mut Pixmap,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    item: &PreparedDraw,
) {
    let pad_x = if item.style == OverlayStyle::Inplace { 6.0 } else { 2.0 };
    let text_max_w = (item.w as f32 - pad_x * 2.0).max(20.0);
    let (buf, _tw, th) = measure_text(font_system, &item.text, item.font_size, Some(text_max_w));

    if item.style == OverlayStyle::Inplace {
        fill_rounded_rect(
            pixmap,
            item.x as f32,
            item.y as f32,
            item.w as f32,
            item.h as f32,
            4.0,
            Color::from_rgba8(14, 15, 22, 245),
        );
        let text_y = item.y + ((item.h as f32 - th) / 2.0).max(1.0) as i32;
        render_text_glyphs(
            pixmap,
            font_system,
            swash_cache,
            &buf,
            item.x + pad_x as i32,
            text_y,
            TextColor::rgb(245, 247, 255),
        );
    } else {
        let tx = item.x + pad_x as i32;
        let ty = item.y;
        let outline = TextColor::rgb(0, 0, 0);
        for (ox, oy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
            render_text_glyphs(pixmap, font_system, swash_cache, &buf, tx + ox, ty + oy, outline);
        }
        render_text_glyphs(
            pixmap,
            font_system,
            swash_cache,
            &buf,
            tx,
            ty,
            TextColor::rgb(255, 255, 255),
        );
    }
}

impl CompositorHandler for WaylandApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {}
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {}
    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {}
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {}
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {}
}

impl OutputHandler for WaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
}

impl LayerShellHandler for WaylandApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {}

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            if self.width != configure.new_size.0 || self.height != configure.new_size.1 {
                self.width = configure.new_size.0;
                self.height = configure.new_size.1;
                self.pool = None;
            }
        }
        self.configured = true;
        self.draw_current();
    }
}

impl ShmHandler for WaylandApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for WaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(WaylandApp);
delegate_output!(WaylandApp);
delegate_shm!(WaylandApp);
delegate_layer!(WaylandApp);
delegate_registry!(WaylandApp);

/// Native Wayland `zwlr_layer_shell_v1` click-through subtitle overlay.
pub struct WaylandLayerOverlay {
    tx: Sender<OverlayCmd>,
    mon_w: i32,
    mon_h: i32,
    font_system: FontSystem,
    worker: Option<JoinHandle<()>>,
}

impl WaylandLayerOverlay {
    pub fn new(mon_w: u32, mon_h: u32) -> Result<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| WaytransError::Overlay(format!("Wayland connection failed: {e}")))?;
        let (globals, mut event_queue) = registry_queue_init(&conn)
            .map_err(|e| WaytransError::Overlay(format!("Wayland registry init failed: {e}")))?;
        let qh = event_queue.handle();

        let compositor_state = CompositorState::bind(&globals, &qh)
            .map_err(|e| WaytransError::Overlay(format!("wl_compositor unavailable: {e}")))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .map_err(|e| WaytransError::Overlay(format!("zwlr_layer_shell_v1 unavailable: {e}")))?;
        let shm = Shm::bind(&globals, &qh)
            .map_err(|e| WaytransError::Overlay(format!("wl_shm unavailable: {e}")))?;

        let surface = compositor_state.create_surface(&qh);
        // Empty input region -> 100% mouse click-through
        if let Ok(empty_region) = Region::new(&compositor_state) {
            surface.set_input_region(Some(empty_region.wl_region()));
        }

        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some("waytrans-hud-overlay"),
            None,
        );
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT | Anchor::BOTTOM);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        let mut app = WaylandApp {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            _compositor_state: compositor_state,
            shm,
            _layer_shell: layer_shell,
            layer,
            pool: None,
            configured: false,
            width: mon_w,
            height: mon_h,
            visible: true,
            current_items: Vec::new(),
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        };

        // Wait for initial configure so surface is immediately drawable
        for _ in 0..8 {
            let _ = event_queue.roundtrip(&mut app);
            if app.configured {
                break;
            }
        }

        let (tx, rx): (Sender<OverlayCmd>, Receiver<OverlayCmd>) = mpsc::channel();
        let worker = thread::spawn(move || loop {
            let _ = event_queue.dispatch_pending(&mut app);
            let _ = conn.flush();

            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(OverlayCmd::Render { items, ack }) => {
                    app.current_items = items;
                    app.draw_current();
                    let _ = conn.flush();
                    let _ = event_queue.roundtrip(&mut app);
                    let _ = ack.send(());
                }
                Ok(OverlayCmd::SetVisible { visible, ack }) => {
                    app.visible = visible;
                    app.draw_current();
                    let _ = conn.flush();
                    let _ = event_queue.roundtrip(&mut app);
                    let _ = ack.send(());
                }
                Ok(OverlayCmd::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        });

        Ok(Self {
            tx,
            mon_w: mon_w as i32,
            mon_h: mon_h as i32,
            font_system: FontSystem::new(),
            worker: Some(worker),
        })
    }
}

impl OverlayRenderer for WaylandLayerOverlay {
    fn update_subtitles(
        &mut self,
        items: &mut [OverlayItem],
        source_obstacles: &[Rect],
        style: OverlayStyle,
        scan_offset: (i32, i32),
    ) -> Result<Vec<Rect>> {
        let items_snapshot = items.to_vec();
        for item in items.iter_mut() {
            let font_px = item.font_size as f32 * 1.15;
            let (_, nat_w, nat_h) = measure_text(&mut self.font_system, &item.trans, font_px, None);
            let avail_w = (self.mon_w - item.pos_x - 10).max(80);
            let src_h = (item.src_bottom_mon - item.src_top_mon).max(0);

            let (render_w, measured_h) = if style == OverlayStyle::Inplace {
                let col_limit =
                    compute_inplace_wrap_width(item, avail_w, &items_snapshot, source_obstacles);
                let target_min_w = if item.src_w > 0 {
                    col_limit.min(item.src_w + 6)
                } else {
                    0
                };
                if (nat_w as i32 + 14) > col_limit {
                    let (_, _, wrap_h) = measure_text(
                        &mut self.font_system,
                        &item.trans,
                        font_px,
                        Some((col_limit - 12).max(40) as f32),
                    );
                    (col_limit, wrap_h as i32 + 6)
                } else {
                    (
                        avail_w.min(target_min_w.max(nat_w as i32 + 14)),
                        nat_h as i32 + 6,
                    )
                }
            } else {
                let rw = avail_w.min(item.max_width.max(nat_w as i32 + 6));
                if (nat_w as i32 + 6) > avail_w {
                    let (_, _, wrap_h) = measure_text(
                        &mut self.font_system,
                        &item.trans,
                        font_px,
                        Some((rw - 4).max(40) as f32),
                    );
                    (rw, wrap_h as i32 + 2)
                } else {
                    (rw, nat_h as i32 + 2)
                }
            };

            let label_h = if style == OverlayStyle::Inplace {
                let item_x0 = item.src_left_mon;
                let item_x1 = item_x0 + item.src_w.max(render_w);
                let next_below_top = items_snapshot
                    .iter()
                    .filter(|other| {
                        other.src_top_mon > item.src_top_mon + 8 && {
                            let ox0 = other.src_left_mon;
                            let ox1 = ox0 + other.src_w.max(other.max_width);
                            item_x0 < ox1 && ox0 < item_x1
                        }
                    })
                    .map(|other| other.src_top_mon)
                    .min();

                let mut effective_src_h = src_h.min(measured_h + 24);
                if let Some(next_top) = next_below_top {
                    effective_src_h = effective_src_h.min((next_top - item.src_top_mon - 3).max(0));
                }
                measured_h.max(effective_src_h)
            } else {
                item.est_height.max(measured_h)
            };
            item.est_height = label_h;
            item.actual_w = render_w;
            item.pos_x = item.pos_x.clamp(4, (self.mon_w - render_w - 4).max(4));

            if style == OverlayStyle::Above {
                let above_y = item.src_top_mon - label_h - 5;
                item.pos_y = if above_y >= 8 {
                    above_y.clamp(4, (self.mon_h - label_h - 8).max(4))
                } else {
                    (item.src_bottom_mon + 5).clamp(4, (self.mon_h - label_h - 8).max(4))
                };
            } else {
                item.pos_y = item.src_top_mon.clamp(4, (self.mon_h - label_h - 8).max(4));
            }
        }

        match style {
            OverlayStyle::Above => {
                resolve_subtitle_collisions(items, self.mon_h, source_obstacles, false);
            }
            OverlayStyle::Inplace => {
                resolve_subtitle_collisions(items, self.mon_h, &[], true);
            }
        }

        let mut prepared = Vec::with_capacity(items.len());
        let mut masks = Vec::with_capacity(items.len());
        for item in items.iter() {
            prepared.push(PreparedDraw {
                x: item.pos_x,
                y: item.pos_y,
                w: item.actual_w,
                h: item.est_height,
                font_size: item.font_size as f32 * 1.15,
                text: item.trans.clone(),
                style,
            });
            masks.push(Rect::from_xywh(
                item.pos_x - scan_offset.0,
                item.pos_y - scan_offset.1,
                item.actual_w,
                item.est_height,
            ));
        }

        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let _ = self.tx.send(OverlayCmd::Render {
            items: prepared,
            ack: ack_tx,
        });
        let _ = ack_rx.recv_timeout(Duration::from_millis(400));
        Ok(masks)
    }

    fn set_visible(&mut self, visible: bool) -> Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let _ = self.tx.send(OverlayCmd::SetVisible {
            visible,
            ack: ack_tx,
        });
        let _ = ack_rx.recv_timeout(Duration::from_millis(400));
        Ok(())
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(OverlayCmd::Shutdown);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WaylandLayerOverlay {
    fn drop(&mut self) {
        self.shutdown();
    }
}
