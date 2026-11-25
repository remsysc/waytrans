use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use libloading::Library;
use once_cell::sync::Lazy;
use regex::Regex;
use waytrans_core::layout::{is_cjk_char, is_cjk_keep_token};
use waytrans_core::{OcrEngine, OcrLine, Result, WaytransError};

static CJK_SPACE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([一-龯ぁ-んァ-ヶ])\s+([一-龯ぁ-んァ-ヶ])").expect("valid regex")
});

const TESSDATA_BEST_URLS: &[(&str, &str)] = &[
    (
        "jpn",
        "https://github.com/tesseract-ocr/tessdata_best/raw/main/jpn.traineddata",
    ),
    (
        "jpn_vert",
        "https://github.com/tesseract-ocr/tessdata_best/raw/main/jpn_vert.traineddata",
    ),
];

const TESSDATA_FAST_URLS: &[(&str, &str)] = &[
    (
        "jpn",
        "https://github.com/tesseract-ocr/tessdata_fast/raw/main/jpn.traineddata",
    ),
    (
        "jpn_vert",
        "https://github.com/tesseract-ocr/tessdata_fast/raw/main/jpn_vert.traineddata",
    ),
];

pub fn tess_lang_for(lang: &str, horizontal_only: bool) -> String {
    let l = lang.to_ascii_lowercase();
    if horizontal_only && matches!(l.as_str(), "ja" | "jp" | "jpn") {
        return "jpn".to_string();
    }
    match l.as_str() {
        "ja" | "jp" | "jpn" => "jpn+jpn_vert".to_string(),
        "zh" => "chi_sim+chi_tra".to_string(),
        "zh-cn" => "chi_sim".to_string(),
        "zh-tw" => "chi_tra".to_string(),
        "ko" | "kr" => "kor".to_string(),
        "ru" => "rus".to_string(),
        "de" => "deu".to_string(),
        "fr" => "fra".to_string(),
        "es" => "spa".to_string(),
        _ => lang.to_string(),
    }
}

pub fn get_tessdata_dir(fast: bool) -> PathBuf {
    if fast {
        if let Ok(p) = std::env::var("WAYTRANS_TESSDATA_FAST_DIR") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    if let Ok(p) = std::env::var("WAYTRANS_TESSDATA_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let data_home = dirs::data_local_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".local/share")
    });
    let folder = if fast { "tessdata_fast" } else { "tessdata" };
    let dir = data_home.join(folder);
    let _ = fs::create_dir_all(&dir);
    dir
}

fn has_models_in_dir(dir: &Path, langs: &[&str]) -> bool {
    langs.iter().all(|name| {
        let p = dir.join(format!("{name}.traineddata"));
        p.exists() && fs::metadata(&p).map(|m| m.len() > 100_000).unwrap_or(false)
    })
}

pub fn ensure_tessdata_dir(tess_lang: &str, fast: bool) -> Option<PathBuf> {
    let sub_langs: Vec<&str> = tess_lang.split('+').map(str::trim).collect();

    if fast {
        let fast_dir = get_tessdata_dir(true);
        if has_models_in_dir(&fast_dir, &sub_langs) {
            return Some(fast_dir);
        }
        if sub_langs.iter().all(|s| *s == "jpn" || *s == "jpn_vert") {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(15))
                .build();
            let mut ok = true;
            for &(name, url) in TESSDATA_FAST_URLS {
                if !sub_langs.contains(&name) {
                    continue;
                }
                let dest = fast_dir.join(format!("{name}.traineddata"));
                if dest.exists() && fs::metadata(&dest).map(|m| m.len() > 500_000).unwrap_or(false) {
                    continue;
                }
                match agent
                    .get(url)
                    .set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) waytrans")
                    .call()
                {
                    Ok(resp) => {
                        let mut reader = resp.into_reader();
                        if let Ok(mut file) = fs::File::create(&dest) {
                            if io::copy(&mut reader, &mut file).is_err() {
                                ok = false;
                            }
                        } else {
                            ok = false;
                        }
                    }
                    Err(_) => {
                        ok = false;
                    }
                }
            }
            if ok && has_models_in_dir(&fast_dir, &sub_langs) {
                return Some(fast_dir);
            }
        }
    }

    let local_dir = get_tessdata_dir(false);
    if has_models_in_dir(&local_dir, &sub_langs) {
        return Some(local_dir);
    }

    if let Ok(prefix) = std::env::var("TESSDATA_PREFIX") {
        let p = PathBuf::from(prefix);
        if has_models_in_dir(&p, &sub_langs) {
            return Some(p);
        }
    }

    for sys_dir in ["/usr/share/tessdata", "/usr/share/tesseract-ocr/5/tessdata", "/usr/share/tesseract-ocr/4.00/tessdata"] {
        let p = Path::new(sys_dir);
        if has_models_in_dir(p, &sub_langs) {
            return Some(p.to_path_buf());
        }
    }

    None
}

pub fn setup_japanese_models() -> Result<()> {
    let target_dir = get_tessdata_dir(false);
    fs::create_dir_all(&target_dir)?;
    println!(
        "Setting up high-accuracy Japanese OCR models in {}...",
        target_dir.display()
    );
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .build();

    for &(name, url) in TESSDATA_BEST_URLS {
        let dest = target_dir.join(format!("{name}.traineddata"));
        if let Ok(meta) = fs::metadata(&dest) {
            if meta.len() > 1_000_000 {
                println!(
                    "  ✓ {name}.traineddata already exists ({} KB)",
                    meta.len() / 1024
                );
                continue;
            }
        }
        println!("  ⬇ Downloading {name}.traineddata from GitHub (tessdata_best)...");
        let resp = agent
            .get(url)
            .set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) waytrans")
            .call()
            .map_err(|e| WaytransError::Ocr(format!("Failed to download {name}.traineddata: {e}")))?;
        let mut reader = resp.into_reader();
        let mut out = fs::File::create(&dest)?;
        io::copy(&mut reader, &mut out)?;
        println!("  ✓ Successfully downloaded {name}.traineddata");
    }
    println!("Model setup complete!");
    Ok(())
}

struct TessApi {
    _lib: Library,
    create: unsafe extern "C" fn() -> *mut c_void,
    delete: unsafe extern "C" fn(*mut c_void),
    end: unsafe extern "C" fn(*mut c_void),
    init3: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    set_psm: unsafe extern "C" fn(*mut c_void, c_int),
    set_var: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    set_image: unsafe extern "C" fn(*mut c_void, *const u8, c_int, c_int, c_int, c_int),
    get_tsv: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_char,
    delete_text: unsafe extern "C" fn(*mut c_char),
}

// SAFETY: Access to the TessBaseAPI handle is serialized via Mutex<TessHandle>;
// TessApi only holds an immutable Library handle and C function pointers.
unsafe impl Send for TessApi {}
unsafe impl Sync for TessApi {}

static TESS_API: OnceLock<TessApi> = OnceLock::new();

fn get_tess_api() -> Result<&'static TessApi> {
    if let Some(api) = TESS_API.get() {
        return Ok(api);
    }
    let api = load_tess_api()?;
    Ok(TESS_API.get_or_init(|| api))
}

struct TessHandle {
    api: &'static TessApi,
    handle: *mut c_void,
    datapath: String,
    lang: String,
    psm: i32,
}

unsafe impl Send for TessHandle {}

impl Drop for TessHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.api.end)(self.handle);
                (self.api.delete)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

fn load_tess_api() -> Result<TessApi> {
    #[cfg(target_os = "windows")]
    let candidates = ["tesseract55.dll", "tesseract54.dll", "tesseract53.dll", "libtesseract-5.dll", "tesseract.dll"];
    #[cfg(not(target_os = "windows"))]
    let candidates = ["libtesseract.so.5", "libtesseract.so"];

    let mut last_err = String::new();
    for name in candidates {
        match unsafe { Library::new(name) } {
            Ok(lib) => unsafe {
                let create = *lib
                    .get::<unsafe extern "C" fn() -> *mut c_void>(b"TessBaseAPICreate\0")
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let delete = *lib
                    .get::<unsafe extern "C" fn(*mut c_void)>(b"TessBaseAPIDelete\0")
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let end = *lib
                    .get::<unsafe extern "C" fn(*mut c_void)>(b"TessBaseAPIEnd\0")
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let init3 = *lib
                    .get::<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int>(
                        b"TessBaseAPIInit3\0",
                    )
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let set_psm = *lib
                    .get::<unsafe extern "C" fn(*mut c_void, c_int)>(b"TessBaseAPISetPageSegMode\0")
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let set_var = *lib
                    .get::<unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int>(
                        b"TessBaseAPISetVariable\0",
                    )
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let set_image = *lib
                    .get::<unsafe extern "C" fn(*mut c_void, *const u8, c_int, c_int, c_int, c_int)>(
                        b"TessBaseAPISetImage\0",
                    )
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let get_tsv = *lib
                    .get::<unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_char>(
                        b"TessBaseAPIGetTsvText\0",
                    )
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;
                let delete_text = *lib
                    .get::<unsafe extern "C" fn(*mut c_char)>(b"TessDeleteText\0")
                    .map_err(|e| WaytransError::Ocr(e.to_string()))?;

                return Ok(TessApi {
                    _lib: lib,
                    create,
                    delete,
                    end,
                    init3,
                    set_psm,
                    set_var,
                    set_image,
                    get_tsv,
                    delete_text,
                });
            },
            Err(e) => {
                last_err = e.to_string();
            }
        }
    }
    Err(WaytransError::Ocr(format!(
        "Failed to load libtesseract shared library ({last_err}). Install tesseract."
    )))
}

fn crop_foreground_gray(gray: &[u8], width: u32, height: u32) -> Option<(Vec<u8>, u32, u32, u32, u32)> {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 || gray.len() < w * h {
        return None;
    }

    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut any_fg = false;

    for y in 0..h {
        let row = &gray[y * w..(y + 1) * w];
        for (x, &px) in row.iter().enumerate() {
            if px < 255 {
                any_fg = true;
                if x < min_x {
                    min_x = x;
                }
                if x > max_x {
                    max_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if y > max_y {
                    max_y = y;
                }
            }
        }
    }

    if !any_fg {
        return None;
    }

    let bx1 = min_x.saturating_sub(16);
    let by1 = min_y.saturating_sub(16);
    let bx2 = (max_x + 1 + 16).min(w);
    let by2 = (max_y + 1 + 16).min(h);

    let crop_w = bx2 - bx1;
    let crop_h = by2 - by1;
    if crop_w * crop_h < ((w * h) as f64 * 0.85) as usize {
        let mut out = Vec::with_capacity(crop_w * crop_h);
        for y in by1..by2 {
            out.extend_from_slice(&gray[y * w + bx1..y * w + bx2]);
        }
        Some((out, crop_w as u32, crop_h as u32, bx1 as u32, by1 as u32))
    } else {
        Some((gray[..w * h].to_vec(), width, height, 0, 0))
    }
}

pub fn parse_tesseract_tsv(
    tsv_output: &str,
    lang: &str,
    min_confidence: f32,
    min_line_confidence: f32,
    coord_scale: f64,
    offset_x: i32,
    offset_y: i32,
) -> Vec<OcrLine> {
    struct RawLine {
        words: Vec<(i32, i32, i32, i32, f32)>,
        text: Vec<String>,
    }

    let mut lines: Vec<RawLine> = Vec::new();
    let mut curr: Option<RawLine> = None;

    for row in tsv_output.lines() {
        let parts: Vec<&str> = row.split('\t').collect();
        if parts.len() < 12 {
            continue;
        }
        match parts[0] {
            "4" => {
                if let Some(c) = curr.take() {
                    if !c.text.is_empty() {
                        lines.push(c);
                    }
                }
                curr = Some(RawLine {
                    words: Vec::new(),
                    text: Vec::new(),
                });
            }
            "5" => {
                if let Some(ref mut c) = curr {
                    let word = parts[11].trim();
                    let conf: f32 = parts[10].parse().unwrap_or(-1.0);
                    if !word.is_empty() && conf >= min_confidence {
                        let w_left: i32 = parts[6].parse().unwrap_or(0);
                        let w_top: i32 = parts[7].parse().unwrap_or(0);
                        let w_width: i32 = parts[8].parse().unwrap_or(0);
                        let w_height: i32 = parts[9].parse().unwrap_or(0);
                        if w_height < 8 || w_width < 3 || w_height > 160 {
                            continue;
                        }
                        if word.chars().count() == 1 && w_height <= 14 && w_width > w_height * 8 {
                            continue;
                        }
                        c.words.push((w_left, w_top, w_left + w_width, w_top + w_height, conf));
                        c.text.push(word.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(c) = curr {
        if !c.text.is_empty() {
            lines.push(c);
        }
    }

    let sl = lang.to_ascii_lowercase();
    let is_cjk = matches!(
        sl.as_str(),
        "ja" | "jp" | "jpn" | "zh" | "chi_sim" | "chi_tra"
    );
    let inv_scale = if coord_scale > 0.0 {
        1.0 / coord_scale
    } else {
        1.0
    };
    let mut results = Vec::new();

    for l in lines {
        if l.words.is_empty() {
            continue;
        }
        if is_cjk {
            let raw_joined: String = l.text.concat();
            let raw_cjk = raw_joined.chars().filter(|&c| is_cjk_char(c)).count();
            let raw_latin = raw_joined.chars().filter(|c| c.is_ascii_alphanumeric()).count();
            if raw_cjk == 0 || (raw_latin > raw_cjk * 2 && raw_cjk < 4) {
                continue;
            }

            let mut kept: Vec<((i32, i32, i32, i32, f32), String)> = l
                .words
                .into_iter()
                .zip(l.text.into_iter())
                .filter(|(_, w)| w.chars().any(is_cjk_keep_token))
                .collect();
            if kept.is_empty() || !kept.iter().any(|(_, w)| w.chars().any(is_cjk_char)) {
                continue;
            }

            kept.sort_by_key(|item| item.0 .0);
            let mut segments: Vec<Vec<((i32, i32, i32, i32, f32), String)>> = vec![vec![kept[0].clone()]];
            for item in kept.into_iter().skip(1) {
                let prev_box = segments.last().and_then(|s| s.last()).map(|x| x.0).unwrap();
                let curr_box = item.0;
                let seg_h = 8.max((prev_box.3 - prev_box.1).max(curr_box.3 - curr_box.1));
                let gutter_thresh = if seg_h >= 28 {
                    seg_h * 5
                } else {
                    24.max((seg_h as f64 * 2.2) as i32)
                };
                if curr_box.0 - prev_box.2 > gutter_thresh {
                    segments.push(vec![item]);
                } else {
                    segments.last_mut().unwrap().push(item);
                }
            }

            for seg in segments {
                if !seg.iter().any(|(_, w)| w.chars().any(is_cjk_char)) {
                    continue;
                }
                let avg_conf = seg.iter().map(|(b, _)| b.4).sum::<f32>() / seg.len() as f32;
                if avg_conf < min_line_confidence {
                    continue;
                }
                let min_x = (seg.iter().map(|(b, _)| b.0).min().unwrap_or(0) as f64 * inv_scale) as i32 + offset_x;
                let min_y = (seg.iter().map(|(b, _)| b.1).min().unwrap_or(0) as f64 * inv_scale) as i32 + offset_y;
                let max_x = (seg.iter().map(|(b, _)| b.2).max().unwrap_or(0) as f64 * inv_scale) as i32 + offset_x;
                let max_y = (seg.iter().map(|(b, _)| b.3).max().unwrap_or(0) as f64 * inv_scale) as i32 + offset_y;

                let mut cleaned: String = seg.into_iter().map(|(_, s)| s).collect();
                cleaned = cleaned.replace(['\r', '\t', '\n'], "");
                cleaned = CJK_SPACE_RE.replace_all(&cleaned, "$1$2").into_owned();
                cleaned = cleaned.replace(' ', "").trim().to_string();
                if !cleaned.is_empty() {
                    results.push(OcrLine {
                        text: cleaned,
                        left: min_x,
                        top: min_y,
                        width: (max_x - min_x).max(1),
                        height: (max_y - min_y).max(1),
                        conf: avg_conf,
                    });
                }
            }
        } else {
            let avg_conf = l.words.iter().map(|b| b.4).sum::<f32>() / l.words.len() as f32;
            if avg_conf < min_line_confidence {
                continue;
            }
            let min_x = (l.words.iter().map(|b| b.0).min().unwrap_or(0) as f64 * inv_scale) as i32 + offset_x;
            let min_y = (l.words.iter().map(|b| b.1).min().unwrap_or(0) as f64 * inv_scale) as i32 + offset_y;
            let max_x = (l.words.iter().map(|b| b.2).max().unwrap_or(0) as f64 * inv_scale) as i32 + offset_x;
            let max_y = (l.words.iter().map(|b| b.3).max().unwrap_or(0) as f64 * inv_scale) as i32 + offset_y;
            let cleaned = l.text.join(" ").trim().to_string();
            if !cleaned.is_empty() {
                results.push(OcrLine {
                    text: cleaned,
                    left: min_x,
                    top: min_y,
                    width: (max_x - min_x).max(1),
                    height: (max_y - min_y).max(1),
                    conf: avg_conf,
                });
            }
        }
    }

    results
}

pub struct TesseractOcr {
    fast_mode: bool,
    horizontal_only: bool,
    state: Mutex<Option<TessHandle>>,
}

impl TesseractOcr {
    pub fn new(fast_mode: bool, horizontal_only: bool) -> Self {
        Self {
            fast_mode,
            horizontal_only,
            state: Mutex::new(None),
        }
    }

    pub fn check_available(&self) -> Result<()> {
        let _ = get_tess_api()?;
        Ok(())
    }

    fn run_tsv(
        &self,
        gray_pixels: &[u8],
        width: u32,
        height: u32,
        datapath: &Path,
        tess_lang: &str,
        psm: i32,
    ) -> Result<(String, u32, u32)> {
        let Some((cropped, cw, ch, cx1, cy1)) = crop_foreground_gray(gray_pixels, width, height) else {
            return Ok((String::new(), 0, 0));
        };

        let mut guard = self
            .state
            .lock()
            .map_err(|_| WaytransError::Ocr("tesseract mutex poisoned".into()))?;

        let dp_str = datapath.to_string_lossy().into_owned();
        let needs_init = match guard.as_ref() {
            Some(st) => st.datapath != dp_str || st.lang != tess_lang,
            None => true,
        };

        if needs_init {
            *guard = None;
            let api = get_tess_api()?;
            let handle = unsafe { (api.create)() };
            if handle.is_null() {
                return Err(WaytransError::Ocr("TessBaseAPICreate returned null".into()));
            }
            let dp_c = CString::new(dp_str.as_str())
                .map_err(|e| WaytransError::Ocr(e.to_string()))?;
            let lang_c = CString::new(tess_lang)
                .map_err(|e| WaytransError::Ocr(e.to_string()))?;
            let rc = unsafe { (api.init3)(handle, dp_c.as_ptr(), lang_c.as_ptr()) };
            if rc != 0 {
                unsafe { (api.delete)(handle) };
                return Err(WaytransError::Ocr(format!(
                    "TessBaseAPIInit3 failed for lang '{tess_lang}' in '{dp_str}'"
                )));
            }
            unsafe {
                (api.set_var)(handle, c"user_defined_dpi".as_ptr(), c"70".as_ptr());
                (api.set_var)(handle, c"tessedit_do_invert".as_ptr(), c"0".as_ptr());
                (api.set_psm)(handle, psm);
            }
            *guard = Some(TessHandle {
                api,
                handle,
                datapath: dp_str,
                lang: tess_lang.to_string(),
                psm,
            });
        } else if let Some(st) = guard.as_mut() {
            if st.psm != psm {
                unsafe { (st.api.set_psm)(st.handle, psm) };
                st.psm = psm;
            }
        }

        let st = guard.as_ref().unwrap();
        let tsv = unsafe {
            (st.api.set_image)(
                st.handle,
                cropped.as_ptr(),
                cw as c_int,
                ch as c_int,
                1,
                cw as c_int,
            );
            let ptr = (st.api.get_tsv)(st.handle, 0);
            if ptr.is_null() {
                return Err(WaytransError::Ocr("TessBaseAPIGetTsvText returned null".into()));
            }
            let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            (st.api.delete_text)(ptr);
            text
        };

        Ok((tsv, cx1, cy1))
    }
}

impl OcrEngine for TesseractOcr {
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
    ) -> Result<Vec<OcrLine>> {
        let tess_lang = tess_lang_for(lang, self.horizontal_only);
        let datapath = ensure_tessdata_dir(&tess_lang, self.fast_mode).ok_or_else(|| {
            WaytransError::Ocr(format!(
                "Could not find traineddata for '{tess_lang}'. Run `waytrans --setup-models`."
            ))
        })?;

        let (tsv, cx1, cy1) = self.run_tsv(gray_pixels, width, height, &datapath, &tess_lang, psm)?;
        let inv_scale = if coord_scale > 0.0 {
            1.0 / coord_scale
        } else {
            1.0
        };
        let off_x = (cx1 as f64 * inv_scale).round() as i32;
        let off_y = (cy1 as f64 * inv_scale).round() as i32;

        Ok(parse_tesseract_tsv(
            &tsv,
            lang,
            min_confidence,
            min_line_confidence,
            coord_scale,
            off_x,
            off_y,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_english_ui_with_stray_cjk_lookalike_and_splits_gutters() {
        let tsv_noise = "\
level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
4\t1\t1\t1\t1\t0\t50\t50\t300\t24\t-1\t
5\t1\t1\t1\t1\t1\t50\t50\t90\t22\t90\tSettings
5\t1\t1\t1\t1\t2\t145\t50\t60\t22\t88\tMenu
5\t1\t1\t1\t1\t3\t210\t50\t20\t22\t80\tロ
5\t1\t1\t1\t1\t4\t235\t50\t40\t22\t85\t123";
        assert!(parse_tesseract_tsv(tsv_noise, "ja", 45.0, 62.0, 1.0, 0, 0).is_empty());

        let tsv_cols = "\
level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
4\t1\t1\t1\t1\t0\t100\t300\t700\t20\t-1\t
5\t1\t1\t1\t1\t1\t100\t300\t180\t18\t90\t日本政府も注視
5\t1\t1\t1\t1\t2\t550\t300\t190\t18\t88\t焼き鳥屋の店主";
        let rows = parse_tesseract_tsv(tsv_cols, "ja", 45.0, 62.0, 1.0, 0, 0);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["日本政府も注視", "焼き鳥屋の店主"]);
    }
}
