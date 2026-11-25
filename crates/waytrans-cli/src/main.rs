mod tray;

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use tracing::{info, warn};
use waytrans_core::diff::{
    border_ring_signature, compute_image_signature, has_significant_change, region_patch_signature,
};
use waytrans_core::layout::compute_smart_placement;
use waytrans_core::tracking::{
    StableReadTracker, StickySubtitle, SubtitleReconciler,
};
use waytrans_core::translator::{detect_block_roles_and_speakers, TranslatorService};
use waytrans_core::{
    boxes_overlap, significant_overlap, DialogueBlock, DialogueTurn, Engine, Frame, OverlayItem,
    OverlayRenderer, OverlayStyle, Rect, ScreenCapture,
};
use waytrans_ocr::{read_blocks_from_frame, setup_japanese_models, TesseractOcr};

#[cfg(target_os = "linux")]
use waytrans_wayland::{
    check_capture_dependencies, copy_to_clipboard, get_active_monitor_geometry, notify_fallback,
    select_region_slurp, GrimCapture, WaylandLayerOverlay,
};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliZone {
    All,
    Bottom,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliStyle {
    Above,
    Inplace,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliEngine {
    Google,
    Openai,
    Gemini,
}

impl From<CliStyle> for OverlayStyle {
    fn from(s: CliStyle) -> Self {
        match s {
            CliStyle::Above => OverlayStyle::Above,
            CliStyle::Inplace => OverlayStyle::Inplace,
        }
    }
}

impl From<CliEngine> for Engine {
    fn from(e: CliEngine) -> Self {
        match e {
            CliEngine::Google => Engine::Google,
            CliEngine::Openai => Engine::OpenAi,
            CliEngine::Gemini => Engine::Gemini,
        }
    }
}

const VERSION_STR: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("WAYTRANS_BUILD_ID"), ")");

#[derive(Parser, Debug)]
#[command(
    name = "waytrans",
    version = VERSION_STR,
    disable_version_flag = true,
    about = "Instant in-place screen OCR translator for Wayland & cross-platform desktops"
)]
struct Args {
    /// Source language for OCR and translation
    #[arg(short = 's', long, default_value = "ja")]
    source: String,

    /// Target language for translation
    #[arg(short = 't', long, default_value = "en")]
    target: String,

    /// Overlay duration in seconds before auto-dismiss (0 = stay until Ctrl-C)
    #[arg(long, default_value_t = 10)]
    timeout: u64,

    /// Tesseract Page Segmentation Mode (0-13)
    #[arg(long, default_value_t = 6)]
    psm: i32,

    /// Disable in-place floating overlay (desktop notification only)
    #[arg(long)]
    no_overlay: bool,

    /// Disable copying translation to clipboard
    #[arg(long)]
    no_clipboard: bool,

    /// Send a desktop notification in addition to the on-screen overlay
    #[arg(long)]
    notify: bool,

    /// Toggle dynamic live translation mode on or off
    #[arg(short = 'l', long)]
    live: bool,

    /// Polling interval in seconds for live translation (min 0.05)
    #[arg(long, default_value_t = 0.15)]
    interval: f64,

    /// Enable full-screen HUD mode without region selection
    #[arg(long, default_value_t = true)]
    fullscreen: bool,

    /// Use interactive region selection instead of fullscreen mode
    #[arg(long)]
    region: bool,

    /// Screen scan zone in live mode
    #[arg(long, value_enum, default_value_t = CliZone::All)]
    zone: CliZone,

    /// Live overlay placement style
    #[arg(long, value_enum, default_value_t = CliStyle::Inplace)]
    style: CliStyle,

    /// Translation engine ('google', 'openai', or 'gemini')
    #[arg(long, env = "WAYTRANS_ENGINE", value_enum, default_value_t = CliEngine::Google)]
    engine: CliEngine,

    /// OpenAI-compatible /v1/chat/completions URL
    #[arg(long, env = "WAYTRANS_API_URL")]
    api_url: Option<String>,

    /// Model name for --engine openai or gemini
    #[arg(long, env = "WAYTRANS_MODEL")]
    model: Option<String>,

    /// Disable system tray icon in live mode
    #[arg(long)]
    no_tray: bool,

    /// Stop any currently running live translation session
    #[arg(long)]
    stop_live: bool,

    /// Download high-accuracy Japanese OCR models into ~/.local/share/tessdata
    #[arg(long)]
    setup_models: bool,

    /// Enable verbose debug logging
    #[arg(short = 'V', long)]
    verbose: bool,

    /// Print version
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: Option<bool>,
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("waytrans")
}

fn pid_file_path() -> PathBuf {
    let dir = cache_dir();
    let _ = fs::create_dir_all(&dir);
    dir.join("live.pid")
}

fn lock_file_path() -> PathBuf {
    let dir = cache_dir();
    let _ = fs::create_dir_all(&dir);
    dir.join("live.pid.lock")
}

#[cfg(target_os = "linux")]
struct LiveLock {
    file: File,
}

#[cfg(target_os = "linux")]
impl Drop for LiveLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(target_os = "linux")]
fn acquire_live_lock() -> Option<LiveLock> {
    use std::os::unix::io::AsRawFd;
    let path = lock_file_path();
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return None;
    }
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();
    Some(LiveLock { file })
}

#[cfg(target_os = "linux")]
fn read_pid_from(path: &PathBuf) -> Option<i32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    let pid: i32 = s.trim().parse().ok()?;
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if alive {
        Some(pid)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn stop_live() -> bool {
    let pid_path = pid_file_path();
    let lock_path = lock_file_path();
    let pid = read_pid_from(&pid_path).or_else(|| read_pid_from(&lock_path));
    let Some(pid) = pid else {
        let _ = fs::remove_file(&pid_path);
        return false;
    };
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        if unsafe { libc::kill(pid, 0) } != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = fs::remove_file(&pid_path);
    notify_fallback("Live HUD Mode", "🛑 Live translation disabled.", "waytrans");
    true
}

fn intrusion_signature(
    style: OverlayStyle,
    sticky: &[StickySubtitle],
    raw_blocks: &[DialogueBlock],
) -> String {
    if style != OverlayStyle::Inplace || sticky.is_empty() || raw_blocks.is_empty() {
        return String::new();
    }
    let mut hits = Vec::new();
    for rb in raw_blocks {
        for s in sticky {
            let horiz_reach = if s.src_rect.height() >= 28 { 500 } else { 180 };
            if rb.top < s.src_rect.bottom + 12
                && rb.bottom > s.src_rect.top - 12
                && rb.left <= s.src_rect.right + horiz_reach
                && rb.right >= s.src_rect.left - 80
            {
                hits.push(format!(
                    "{}@{},{}",
                    rb.combined_text.trim(),
                    rb.left / 16,
                    rb.top / 16
                ));
                break;
            }
        }
    }
    hits.join("|")
}

fn inplace_border_unchanged(
    frame: &Frame,
    sub: &StickySubtitle,
    active_masks: &[Rect],
) -> bool {
    let Some(ref prev_sig) = sub.border_sig else {
        return false;
    };
    let Some(curr_sig) = border_ring_signature(frame, sub.src_rect, 20, active_masks) else {
        return false;
    };
    !has_significant_change(prev_sig, &curr_sig, 420, 14, 8)
}

fn source_patch_unchanged(frame: &Frame, sub: &StickySubtitle) -> bool {
    let Some(ref prev_sig) = sub.src_patch_sig else {
        return false;
    };
    let Some(curr_sig) = region_patch_signature(frame, sub.src_rect) else {
        return false;
    };
    !has_significant_change(prev_sig, &curr_sig, 200, 12, 6)
}

fn capture_unmasked_inplace_frame(
    capture: &mut dyn ScreenCapture,
    overlay: &mut dyn OverlayRenderer,
    sticky: &[StickySubtitle],
) -> Option<Frame> {
    if sticky.is_empty() {
        return capture.capture().ok();
    }
    let frame_before = capture.capture().ok()?;
    let pre_sigs: Vec<(&StickySubtitle, Vec<u8>)> = sticky
        .iter()
        .filter_map(|s| region_patch_signature(&frame_before, s.src_rect).map(|sig| (s, sig)))
        .collect();

    let _ = overlay.set_visible(false);
    let mut revealed_frame: Option<Frame> = None;
    for _ in 0..2 {
        thread::sleep(Duration::from_millis(25));
        let Ok(candidate) = capture.capture() else {
            continue;
        };
        if pre_sigs.is_empty()
            || pre_sigs.iter().any(|(s, pre_sig)| {
                region_patch_signature(&candidate, s.src_rect)
                    .map(|curr| has_significant_change(pre_sig, &curr, 120, 8, 3))
                    .unwrap_or(false)
            })
        {
            revealed_frame = Some(candidate);
            break;
        }
    }
    let _ = overlay.set_visible(true);
    revealed_frame
}

#[cfg(target_os = "linux")]
fn run_live_loop(args: &Args) -> i32 {
    let Some(_lock) = acquire_live_lock() else {
        stop_live();
        println!("Live translation stopped.");
        return 0;
    };
    if read_pid_from(&pid_file_path()).is_some() {
        drop(_lock);
        stop_live();
        println!("Live translation stopped.");
        return 0;
    }

    let pid_path = pid_file_path();
    let _ = fs::write(&pid_path, std::process::id().to_string());

    let use_fullscreen = !args.region;
    if let Err(e) = check_capture_dependencies(!use_fullscreen) {
        notify_fallback("Live Mode Error", &e.to_string(), "waytrans");
        let _ = fs::remove_file(&pid_path);
        return 1;
    }

    let ocr = TesseractOcr::new(true, true);
    if let Err(e) = ocr.check_available() {
        notify_fallback("Live Mode Error", &e.to_string(), "waytrans");
        let _ = fs::remove_file(&pid_path);
        return 1;
    }

    let (mon_x, mon_y, mon_w, mon_h) = get_active_monitor_geometry();
    let (scan_x, scan_y, scan_w, scan_h) = if use_fullscreen {
        match args.zone {
            CliZone::Bottom => {
                let sh = ((mon_h as f64 * 0.35) as u32).max(1);
                (mon_x, mon_y + (mon_h - sh) as i32, mon_w, sh)
            }
            CliZone::All => (mon_x, mon_y, mon_w, mon_h),
        }
    } else {
        notify_fallback("Live Mode", "Select region to watch...", "waytrans");
        match select_region_slurp("#a6e3a1", "#1e1e2e66") {
            Ok(Some((rx, ry, rw, rh))) => {
                if rw < 25 || rh < 15 {
                    notify_fallback("Live Mode", "Selected region was too small.", "waytrans");
                    let _ = fs::remove_file(&pid_path);
                    return 0;
                }
                (rx, ry, rw, rh)
            }
            Ok(None) => {
                println!("Selection cancelled.");
                let _ = fs::remove_file(&pid_path);
                return 0;
            }
            Err(e) => {
                notify_fallback("Capture Error", &e.to_string(), "waytrans");
                let _ = fs::remove_file(&pid_path);
                return 1;
            }
        }
    };

    let style: OverlayStyle = args.style.into();
    let engine: Engine = args.engine.into();
    let style_desc = match style {
        OverlayStyle::Above => "above original text (learning mode)",
        OverlayStyle::Inplace => "in-place overlay",
    };
    notify_fallback(
        "Game HUD Mode Active",
        &format!(
            "🎮 Monitoring ({scan_w}x{scan_h}) [{style_desc} | {}].\nPress Super+Alt+L to stop.",
            engine.as_str()
        ),
        &format!("waytrans v{}", env!("CARGO_PKG_VERSION")),
    );

    let mut capture = GrimCapture::new(scan_x, scan_y, scan_w, scan_h, mon_x, mon_y, mon_w, mon_h);
    let mut overlay = match WaylandLayerOverlay::new(mon_w, mon_h) {
        Ok(o) => o,
        Err(e) => {
            notify_fallback("Overlay Error", &e.to_string(), "waytrans");
            let _ = fs::remove_file(&pid_path);
            return 1;
        }
    };

    let translator = TranslatorService::new(
        engine,
        args.api_url.clone(),
        args.model.clone(),
        None,
    );
    translator.load_persistent_cache();

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = Arc::clone(&running);
        let _ = ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        });
    }

    let tray_state = tray::TrayState {
        enabled: Arc::new(AtomicBool::new(true)),
        running: Arc::clone(&running),
        style: Arc::new(Mutex::new(style)),
    };
    let tray_guard = if !args.no_tray {
        tray::start_tray(tray_state.clone())
    } else {
        None
    };

    let base_interval = Duration::from_secs_f64(args.interval.max(0.05));
    let effective_psm = if !use_fullscreen || matches!(args.zone, CliZone::Bottom) {
        6
    } else {
        11
    };

    let mut tracker = StableReadTracker::new(2, 0.85);
    let reconciler = SubtitleReconciler::new(2);
    let mut sticky_subtitles: Vec<StickySubtitle> = Vec::new();
    let mut dialogue_history: Vec<DialogueTurn> = Vec::new();
    let mut active_mask_rects: Vec<Rect> = Vec::new();
    let mut last_img_bytes: Option<Vec<u8>> = None;
    let mut last_text_hash = String::new();
    let mut pending_scan = false;
    let mut needs_followup_scan = false;
    let mut next_retry_at: Option<Instant> = None;
    let mut next_inplace_peek_at = Instant::now();
    let mut inplace_peek_backoff = 0.0_f64;
    let mut last_intrusion_sig = String::new();
    let mut was_enabled = true;
    let mut active_style = style;

    let scan_offset = (scan_x - mon_x, scan_y - mon_y);

    while running.load(Ordering::SeqCst) {
        let is_enabled = tray_state.enabled.load(Ordering::Relaxed);
        if !is_enabled {
            if was_enabled {
                was_enabled = false;
                let _ = overlay.set_visible(false);
                if let Some(ref tg) = tray_guard {
                    tg.refresh();
                }
            }
            thread::sleep(Duration::from_millis(100));
            continue;
        } else if !was_enabled {
            was_enabled = true;
            let _ = overlay.set_visible(true);
            last_img_bytes = None;
            last_text_hash.clear();
            if let Some(ref tg) = tray_guard {
                tg.refresh();
            }
        }

        let current_style = *tray_state.style.lock().unwrap();
        if current_style != active_style {
            active_style = current_style;
            last_img_bytes = None;
            last_text_hash.clear();
            sticky_subtitles.clear();
            active_mask_rects.clear();
        }
        let style = active_style;

        let Ok(mut frame) = capture.capture() else {
            thread::sleep(base_interval);
            continue;
        };

        let mut current_masks = active_mask_rects.clone();
        let img_sig = compute_image_signature(&frame, &current_masks);
        let retry_due = next_retry_at.map(|t| Instant::now() >= t).unwrap_or(false);
        let must_scan = pending_scan || needs_followup_scan || retry_due;

        if !must_scan {
            if let Some(ref prev) = last_img_bytes {
                if !has_significant_change(prev, &img_sig, 450, 12, 8) {
                    thread::sleep(base_interval);
                    continue;
                }
            }
        }
        last_img_bytes = Some(img_sig);

        let mut hid_for_inplace = false;
        let mut attempted_inplace_peek = false;
        if style == OverlayStyle::Inplace
            && !sticky_subtitles.is_empty()
            && Instant::now() >= next_inplace_peek_at
            && !sticky_subtitles
                .iter()
                .all(|s| inplace_border_unchanged(&frame, s, &current_masks))
        {
            attempted_inplace_peek = true;
            if let Some(clean_frame) =
                capture_unmasked_inplace_frame(&mut capture, &mut overlay, &sticky_subtitles)
            {
                frame = clean_frame;
                hid_for_inplace = true;
                current_masks.clear();
            }
        }

        let build_ocr_masks = |masks: &[Rect]| -> Vec<Rect> {
            let mut out = masks.to_vec();
            if style == OverlayStyle::Inplace && !out.is_empty() {
                for s in &sticky_subtitles {
                    out.push(s.src_rect);
                }
            }
            out
        };

        let ocr_masks = if hid_for_inplace {
            Vec::new()
        } else {
            build_ocr_masks(&current_masks)
        };

        let mut raw_blocks = match read_blocks_from_frame(
            &ocr,
            &frame,
            &args.source,
            effective_psm,
            &ocr_masks,
            30.0,
            62.0,
        ) {
            Ok(b) => b,
            Err(e) => {
                warn!("OCR failed: {e}");
                thread::sleep(base_interval);
                continue;
            }
        };

        let (mut stable_blocks, p_scan) = tracker.update(&raw_blocks, |b| {
            let txt = b.combined_text.trim();
            txt.chars().count() >= 4
                && translator.is_translation_cached(txt, &args.source, &args.target)
        });
        pending_scan = p_scan;

        let mut rec = reconciler.reconcile(
            &sticky_subtitles,
            &raw_blocks,
            &stable_blocks,
            if !hid_for_inplace && style == OverlayStyle::Inplace {
                Some(|s: &StickySubtitle| inplace_border_unchanged(&frame, s, &current_masks))
            } else {
                None
            },
            Some(|s: &StickySubtitle| source_patch_unchanged(&frame, s)),
            Some(|s: &StickySubtitle| region_patch_signature(&frame, s.src_rect)),
        );

        let intr_sig = intrusion_signature(style, &sticky_subtitles, &raw_blocks);
        if !hid_for_inplace
            && !intr_sig.is_empty()
            && (intr_sig != last_intrusion_sig || Instant::now() >= next_inplace_peek_at)
        {
            last_intrusion_sig = intr_sig;
            attempted_inplace_peek = true;
            if let Some(clean_frame) =
                capture_unmasked_inplace_frame(&mut capture, &mut overlay, &sticky_subtitles)
            {
                frame = clean_frame;
                hid_for_inplace = true;
                current_masks.clear();
                let _ = hid_for_inplace;
                if let Ok(rb) = read_blocks_from_frame(
                    &ocr,
                    &frame,
                    &args.source,
                    effective_psm,
                    &[],
                    30.0,
                    62.0,
                ) {
                    raw_blocks = rb;
                    let (sb, p2) = tracker.update(&raw_blocks, |b| {
                        let txt = b.combined_text.trim();
                        txt.chars().count() >= 4
                            && translator.is_translation_cached(txt, &args.source, &args.target)
                    });
                    stable_blocks = sb;
                    pending_scan = p2;
                    rec = reconciler.reconcile(
                        &sticky_subtitles,
                        &raw_blocks,
                        &stable_blocks,
                        None::<fn(&StickySubtitle) -> bool>,
                        Some(|s: &StickySubtitle| source_patch_unchanged(&frame, s)),
                        Some(|s: &StickySubtitle| region_patch_signature(&frame, s.src_rect)),
                    );
                }
            }
        }

        let mut surviving = rec.surviving;
        needs_followup_scan = rec.needs_followup_scan;

        if !rec.new_blocks.is_empty() {
            let context_blocks = if stable_blocks.len() >= rec.new_blocks.len() {
                &stable_blocks
            } else {
                &rec.new_blocks
            };
            let all_translated = translator.translate_contextual_blocks(
                context_blocks,
                scan_h as i32,
                &dialogue_history,
                &args.source,
                &args.target,
            );

            let metas = detect_block_roles_and_speakers(context_blocks, scan_h as i32);
            for (meta, tr) in metas.iter().zip(all_translated.iter()) {
                if meta.role == "speaker"
                    || meta.text.chars().count() < 4
                    || tr.trim().is_empty()
                {
                    continue;
                }
                let already_last = dialogue_history
                    .last()
                    .map(|h| h.source == meta.text)
                    .unwrap_or(false);
                let in_recent = dialogue_history
                    .iter()
                    .rev()
                    .take(3)
                    .any(|h| h.source == meta.text);
                if !already_last && !in_recent {
                    dialogue_history.push(DialogueTurn {
                        speaker: meta.speaker.clone(),
                        source: meta.text.clone(),
                        translation: tr.trim().to_string(),
                    });
                }
            }
            if dialogue_history.len() > 10 {
                let drop_n = dialogue_history.len() - 10;
                dialogue_history.drain(0..drop_n);
            }

            let mut any_failed = false;
            for block in &rec.new_blocks {
                let trans = context_blocks
                    .iter()
                    .zip(all_translated.iter())
                    .find(|(cb, _)| cb.combined_text == block.combined_text && cb.top == block.top)
                    .map(|(_, tr)| tr.as_str())
                    .unwrap_or("");
                if trans.trim().is_empty() {
                    any_failed = true;
                    continue;
                }
                let (pos_x, pos_y, font_size, max_w, est_h) = compute_smart_placement(
                    block,
                    trans,
                    mon_w as i32,
                    mon_h as i32,
                    scan_offset,
                    style,
                );
                surviving.retain(|s| !significant_overlap(s.src_rect, block.rect(), 0.3));
                let patch_sig = region_patch_signature(&frame, block.rect());
                let border_sig = if style == OverlayStyle::Inplace {
                    border_ring_signature(&frame, block.rect(), 20, &[])
                } else {
                    None
                };
                surviving.push(StickySubtitle {
                    source_text: block.combined_text.clone(),
                    trans: trans.to_string(),
                    src_rect: block.rect(),
                    pos_x,
                    pos_y,
                    font_size,
                    max_width: max_w,
                    est_height: est_h,
                    src_patch_sig: patch_sig,
                    border_sig,
                    miss_count: 0,
                });
            }

            if any_failed {
                next_retry_at = Some(Instant::now() + Duration::from_millis(750));
                needs_followup_scan = true;
            } else {
                next_retry_at = None;
                translator.save_persistent_cache();
            }
        }

        sticky_subtitles = surviving;

        let mut source_obstacles: Vec<Rect> = sticky_subtitles
            .iter()
            .map(|s| {
                Rect::from_xywh(
                    scan_offset.0 + s.src_rect.left,
                    scan_offset.1 + s.src_rect.top,
                    s.src_rect.width(),
                    s.src_rect.height(),
                )
            })
            .collect();
        for rb in &raw_blocks {
            source_obstacles.push(Rect::from_xywh(
                scan_offset.0 + rb.left,
                scan_offset.1 + rb.top,
                rb.width(),
                rb.height(),
            ));
        }

        let mask_hits_source = style == OverlayStyle::Above
            && !current_masks.is_empty()
            && current_masks
                .iter()
                .any(|&m| raw_blocks.iter().any(|rb| boxes_overlap(m, rb.rect(), 12)));

        let text_sig: String = sticky_subtitles
            .iter()
            .map(|s| format!("{}@{},{}:{}", s.source_text, s.pos_x / 8, s.pos_y / 8, s.trans))
            .collect::<Vec<_>>()
            .join("|");

        if text_sig == last_text_hash && !mask_hits_source {
            if attempted_inplace_peek {
                thread::sleep(Duration::from_millis(85));
                if let Ok(post_frame) = capture.capture() {
                    last_img_bytes = Some(compute_image_signature(&post_frame, &active_mask_rects));
                    if style == OverlayStyle::Inplace {
                        for s in &mut sticky_subtitles {
                            s.border_sig = border_ring_signature(
                                &post_frame,
                                s.src_rect,
                                20,
                                &active_mask_rects,
                            );
                        }
                    }
                }
                inplace_peek_backoff = (inplace_peek_backoff * 2.0).max(1.2).min(6.0);
                next_inplace_peek_at =
                    Instant::now() + Duration::from_secs_f64(inplace_peek_backoff);
            }
            let wait = if pending_scan || needs_followup_scan {
                base_interval.min(Duration::from_millis(30))
            } else {
                base_interval
            };
            thread::sleep(wait);
            continue;
        }

        if mask_hits_source {
            needs_followup_scan = true;
        }
        last_text_hash = text_sig;
        inplace_peek_backoff = 0.0;
        last_intrusion_sig.clear();
        next_inplace_peek_at = if style == OverlayStyle::Inplace && !sticky_subtitles.is_empty() {
            Instant::now() + Duration::from_millis(400)
        } else {
            Instant::now()
        };

        let mut items_to_render: Vec<OverlayItem> = sticky_subtitles
            .iter()
            .map(|s| OverlayItem {
                pos_x: s.pos_x,
                pos_y: s.pos_y,
                font_size: s.font_size,
                max_width: s.max_width,
                est_height: s.est_height,
                actual_w: s.max_width,
                trans: s.trans.clone(),
                src_left_mon: scan_offset.0 + s.src_rect.left,
                src_w: s.src_rect.width(),
                src_top_mon: scan_offset.1 + s.src_rect.top,
                src_bottom_mon: scan_offset.1 + s.src_rect.bottom,
            })
            .collect();

        let prev_mask_count = current_masks.len();
        if let Ok(new_masks) =
            overlay.update_subtitles(&mut items_to_render, &source_obstacles, style, scan_offset)
        {
            active_mask_rects = new_masks;
        }

        thread::sleep(Duration::from_millis(85));
        if let Ok(post_frame) = capture.capture() {
            last_img_bytes = Some(compute_image_signature(&post_frame, &active_mask_rects));
            if style == OverlayStyle::Inplace {
                for s in &mut sticky_subtitles {
                    s.border_sig =
                        border_ring_signature(&post_frame, s.src_rect, 20, &active_mask_rects);
                }
            }
        }
        if active_mask_rects.len() < prev_mask_count {
            needs_followup_scan = true;
        }

        let wait = if pending_scan || needs_followup_scan {
            base_interval.min(Duration::from_millis(30))
        } else {
            base_interval
        };
        thread::sleep(wait);
    }

    overlay.shutdown();
    translator.save_persistent_cache();
    let _ = fs::remove_file(&pid_path);
    info!("Live translation stopped cleanly");
    0
}

#[cfg(target_os = "linux")]
fn run_oneshot(args: &Args) -> i32 {
    let region = match select_region_slurp("#89b4fa", "#1e1e2e88") {
        Ok(Some(r)) => r,
        Ok(None) => return 0,
        Err(e) => {
            eprintln!("Capture error: {e}");
            notify_fallback("Capture Error", &e.to_string(), "waytrans");
            return 1;
        }
    };
    let (rx, ry, rw, rh) = region;
    let (mon_x, mon_y, mon_w, mon_h) = get_active_monitor_geometry();
    let mut capture = GrimCapture::new(rx, ry, rw, rh, mon_x, mon_y, mon_w, mon_h);
    let frame = match capture.capture() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Capture error: {e}");
            return 1;
        }
    };

    let ocr = TesseractOcr::new(false, false);
    let blocks = match read_blocks_from_frame(&ocr, &frame, &args.source, args.psm, &[], 45.0, 62.0) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("OCR error: {e}");
            notify_fallback("OCR Error", &e.to_string(), "waytrans");
            return 1;
        }
    };
    if blocks.is_empty() {
        notify_fallback("", "No text detected in selected region.", "waytrans");
        return 0;
    }

    let source_text: String = blocks
        .iter()
        .map(|b| b.combined_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let translator = TranslatorService::new(
        args.engine.into(),
        args.api_url.clone(),
        args.model.clone(),
        None,
    );
    let translated = match translator.translate_single(&source_text, &args.source, &args.target) {
        Ok(t) if !t.trim().is_empty() => t,
        Ok(_) => {
            notify_fallback(&source_text, "Translation returned empty result.", "waytrans");
            return 0;
        }
        Err(e) => {
            eprintln!("Translation error: {e}");
            notify_fallback("Translation Error", &e.to_string(), "waytrans");
            return 1;
        }
    };

    if !args.no_clipboard {
        copy_to_clipboard(&translated);
    }
    if args.notify || args.no_overlay {
        notify_fallback(
            &source_text,
            &translated,
            &format!("waytrans [{}➔{}]", args.source, args.target),
        );
    }
    if !args.no_overlay {
        if let Ok(mut overlay) = WaylandLayerOverlay::new(mon_w, mon_h) {
            let mut items = vec![OverlayItem {
                pos_x: (rx - mon_x).max(8),
                pos_y: (ry - mon_y).max(8),
                font_size: 16,
                max_width: rw.min(850) as i32,
                est_height: 40,
                actual_w: rw.min(850) as i32,
                trans: translated,
                src_left_mon: rx - mon_x,
                src_w: rw as i32,
                src_top_mon: ry - mon_y,
                src_bottom_mon: (ry - mon_y) + rh as i32,
            }];
            let _ = overlay.update_subtitles(&mut items, &[], OverlayStyle::Inplace, (rx - mon_x, ry - mon_y));
            let wait_secs = if args.timeout == 0 { 10 } else { args.timeout };
            thread::sleep(Duration::from_secs(wait_secs));
            overlay.shutdown();
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn run_live_loop(_args: &Args) -> i32 {
    eprintln!("Windows live capture/overlay backend is not yet implemented.");
    1
}

#[cfg(not(target_os = "linux"))]
fn run_oneshot(_args: &Args) -> i32 {
    eprintln!("Windows oneshot capture/overlay backend is not yet implemented.");
    1
}

#[cfg(not(target_os = "linux"))]
fn stop_live() -> bool {
    false
}

fn main() {
    let args = Args::parse();
    if args.interval < 0.05 {
        eprintln!("error: --interval must be at least 0.05 seconds.");
        std::process::exit(2);
    }
    if !(0..=13).contains(&args.psm) {
        eprintln!("error: --psm must be between 0 and 13.");
        std::process::exit(2);
    }

    let filter = if args.verbose {
        "debug"
    } else {
        "warn"
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .try_init();

    if args.stop_live {
        if stop_live() {
            println!("Live translation stopped.");
        } else {
            println!("No active live translation session found.");
        }
        return;
    }

    if args.setup_models {
        if let Err(e) = setup_japanese_models() {
            eprintln!("Error setting up models: {e}");
            std::process::exit(1);
        }
        return;
    }

    let code = if args.live {
        run_live_loop(&args)
    } else {
        run_oneshot(&args)
    };
    std::process::exit(code);
}
