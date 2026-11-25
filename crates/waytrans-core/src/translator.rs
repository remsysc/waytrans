use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use tracing::debug;

use crate::layout::is_cjk_char;
use crate::types::{DialogueBlock, DialogueTurn, Engine, Result, WaytransError};

static CACHE_TRAILING_PUNCT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[…・.。、\s]+$").expect("valid regex"));
static CONTEXT_PREFIX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:Context|Background|Question|Option|Choice|Line|Lines|Dialogue|Prompt|Speech|Serif|Subtitle|Speaker|Message|Text|Translation|Q|A)\s*\d*\s*[:：\-—]\s*",
    )
    .expect("valid regex")
});
static JA_HESITANT_QUESTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(みようか)(?:…+|[・.]{2,})$").expect("valid regex"));
static DUP_THE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bthe\s+the\b").expect("valid regex"));
static MULTI_NL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*\n+\s*").expect("valid regex"));
static PIPE_SPLIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*\|\|\|\s*").expect("valid regex"));
static JSON_EXTRACT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(\[[\s\S]*\]|\{[\s\S]*\})").expect("valid regex"));
static LLM_LEADING_TAG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:\d+[.)]\s*|\[[^\]]{1,24}\]\s*)+").expect("valid regex"));

const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

const GAME_GLOSSARY_JA_EN: &[(&str, &str)] = &[
    ("マヨナカテレビ", "the Midnight Channel"),
    ("セーブポイント", "Save Point"),
    ("セーポイント", "Save Point"),
    ("ベルベットルーム", "the Velvet Room"),
    ("ジュネス", "Junes"),
    ("八十神高校", "Yasogami High"),
    ("八十稲羽", "Yasoinaba"),
    ("稲羽市", "Inaba"),
    ("特別捜査隊", "the Investigation Team"),
    ("自称特別捜査隊", "the Investigation Team"),
    ("コミュ", "Social Link"),
    ("ペルソナ", "Persona"),
    ("シャドウ", "Shadow"),
    ("総攻撃", "All-Out Attack"),
    ("ペルソナ全書", "the Persona Compendium"),
    ("アルカナ", "Arcana"),
];

pub struct BoundedLruCache {
    maxsize: usize,
    inner: Mutex<LruState>,
}

struct LruState {
    map: HashMap<String, String>,
    order: VecDeque<String>,
}

impl BoundedLruCache {
    pub fn new(maxsize: usize) -> Self {
        Self {
            maxsize: maxsize.max(1),
            inner: Mutex::new(LruState {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        let mut guard = self.inner.lock().ok()?;
        let val = guard.map.get(key)?.clone();
        guard.order.retain(|k| k != key);
        guard.order.push_back(key.to_string());
        Some(val)
    }

    pub fn set(&self, key: String, value: String) {
        if value.is_empty() {
            return;
        }
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if guard.map.contains_key(&key) {
            guard.order.retain(|k| k != &key);
        }
        guard.map.insert(key.clone(), value);
        guard.order.push_back(key);
        while guard.map.len() > self.maxsize {
            if let Some(oldest) = guard.order.pop_front() {
                guard.map.remove(&oldest);
            }
        }
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.inner
            .lock()
            .map(|g| g.map.clone())
            .unwrap_or_default()
    }
}

fn default_cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("waytrans")
        .join("translations.json")
}

fn default_glossary_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("waytrans")
        .join("glossary.json")
}

fn normalize_cache_text(text: &str) -> String {
    CACHE_TRAILING_PUNCT_RE.replace(text.trim(), "").into_owned()
}

pub fn apply_game_glossary(text: &str, source_lang: &str, target_lang: &str) -> String {
    if !source_lang.to_ascii_lowercase().starts_with("ja")
        || !target_lang.to_ascii_lowercase().starts_with("en")
    {
        return text.to_string();
    }
    let mut out = text.to_string();
    for &(jp, en) in GAME_GLOSSARY_JA_EN {
        if out.contains(jp) {
            out = out.replace(jp, en);
        }
    }
    out
}

pub fn prepare_source_text(text: &str, source_lang: &str, target_lang: &str) -> String {
    let out = apply_game_glossary(text, source_lang, target_lang);
    if source_lang.to_ascii_lowercase().starts_with("ja") {
        JA_HESITANT_QUESTION_RE.replace(&out, "${1}？...").into_owned()
    } else {
        out
    }
}

pub fn clean_translated_text(text: &str) -> String {
    let mut cleaned = text.trim().replace("? ...", "?...");
    cleaned = DUP_THE_RE.replace_all(&cleaned, "the").into_owned();
    cleaned
}

fn strip_outer_quotes(s: &str) -> &str {
    let t = s.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() >= 2
        && matches!(chars[0], '"' | '“')
        && matches!(chars[chars.len() - 1], '"' | '”')
    {
        let start = chars[0].len_utf8();
        let end = t.len() - chars[chars.len() - 1].len_utf8();
        t[start..end].trim()
    } else {
        t
    }
}

pub fn load_user_glossary(path: Option<&Path>) -> HashMap<String, String> {
    let target = path.map(PathBuf::from).unwrap_or_else(default_glossary_path);
    let Ok(raw) = fs::read_to_string(&target) else {
        return HashMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) else {
        return HashMap::new();
    };
    let mut merged = HashMap::new();
    for section in ["characters", "terms"] {
        if let Some(Value::Object(sub)) = map.get(section) {
            for (k, v) in sub {
                if let Some(s) = v.as_str() {
                    if !k.trim().is_empty() && !s.trim().is_empty() {
                        merged.insert(k.trim().to_string(), s.trim().to_string());
                    }
                }
            }
        }
    }
    for (k, v) in map {
        if k != "characters" && k != "terms" {
            if let Some(s) = v.as_str() {
                if !k.trim().is_empty() && !s.trim().is_empty() {
                    merged.insert(k.trim().to_string(), s.trim().to_string());
                }
            }
        }
    }
    merged
}

#[derive(Debug, Clone)]
pub struct BlockMeta {
    pub index: usize,
    pub text: String,
    pub role: &'static str,
    pub speaker: String,
}

pub fn detect_block_roles_and_speakers(
    blocks: &[DialogueBlock],
    viewport_height: i32,
) -> Vec<BlockMeta> {
    if blocks.is_empty() {
        return Vec::new();
    }
    let bottom_threshold = if viewport_height > 200 {
        (viewport_height as f64 * 0.64) as i32
    } else {
        650
    };

    let mut metas: Vec<BlockMeta> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| BlockMeta {
            index: i,
            text: b.combined_text.trim().to_string(),
            role: "dialogue",
            speaker: String::new(),
        })
        .collect();

    let mut speaker_indices = Vec::new();
    for i in 0..blocks.len() {
        let sp = metas[i].text.clone();
        let char_len = sp.chars().count();
        if sp.is_empty() || sp.contains('\n') || char_len > 8 {
            continue;
        }
        if sp
            .chars()
            .last()
            .map(|c| matches!(c, '。' | '！' | '？' | '!' | '?' | '…'))
            .unwrap_or(false)
        {
            continue;
        }
        let max_vert_gap = ((blocks[i].height() as f64 * 2.5) as i32).max(110);
        let mut child_indices = Vec::new();
        for j in 0..blocks.len() {
            if i == j {
                continue;
            }
            let gap = blocks[j].top - blocks[i].bottom;
            if blocks[j].top > blocks[i].top
                && (0..=max_vert_gap).contains(&gap)
                && (blocks[j].left - blocks[i].left).abs() <= 120
                && (metas[j].text.chars().count() > char_len
                    || (blocks[j].width() as f64) > (blocks[i].width() as f64) * 1.3)
            {
                child_indices.push(j);
            }
        }
        if !child_indices.is_empty() {
            metas[i].role = "speaker";
            speaker_indices.push(i);
            for j in child_indices {
                if metas[j].speaker.is_empty() {
                    metas[j].speaker = sp.clone();
                }
            }
        }
    }

    let mut prompt_count = 0usize;
    let mut choice_count = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        if speaker_indices.contains(&i) {
            continue;
        }
        if b.top >= bottom_threshold || b.width() > 580 {
            metas[i].role = "prompt";
            prompt_count += 1;
        } else {
            metas[i].role = "choice";
            choice_count += 1;
        }
    }
    if !(prompt_count > 0 && choice_count > 0) {
        for (i, m) in metas.iter_mut().enumerate() {
            if !speaker_indices.contains(&i) {
                m.role = "dialogue";
            }
        }
    }
    metas
}

pub fn parse_llm_json_array(raw_text: &str, expected_len: usize) -> Option<Vec<String>> {
    if raw_text.trim().is_empty() || expected_len == 0 {
        return None;
    }
    let mut cleaned = raw_text.trim().to_string();
    if cleaned.starts_with("```") {
        if let Some(nl) = cleaned.find('\n') {
            cleaned = cleaned[nl + 1..].to_string();
        }
        if let Some(end) = cleaned.rfind("```") {
            cleaned = cleaned[..end].trim().to_string();
        }
    }

    let mut parsed: Option<Value> = serde_json::from_str(&cleaned).ok();
    if parsed.is_none() {
        if let Some(m) = JSON_EXTRACT_RE.find(&cleaned) {
            parsed = serde_json::from_str(m.as_str()).ok();
        }
    }

    let arr = match parsed? {
        Value::Array(a) => a,
        Value::Object(mut obj) => match obj.remove("translations") {
            Some(Value::Array(a)) => a,
            _ => return None,
        },
        _ => return None,
    };

    if arr.len() != expected_len {
        return None;
    }
    let mut out = Vec::with_capacity(expected_len);
    for v in arr {
        let s = v.as_str()?.trim();
        if s.is_empty() {
            return None;
        }
        out.push(clean_translated_text(s));
    }
    Some(out)
}

fn is_cjk_lang(lang: &str) -> bool {
    let l = lang.to_ascii_lowercase();
    l.starts_with("ja") || l.starts_with("jp") || l.starts_with("zh") || l.starts_with("ko")
}

pub fn validate_llm_translations(
    out: &mut [String],
    source_lang: &str,
    target_lang: &str,
) -> bool {
    let reject_cjk_echo = is_cjk_lang(source_lang) && !is_cjk_lang(target_lang);
    for item in out.iter_mut() {
        let stripped = LLM_LEADING_TAG_RE.replace(item.trim(), "").into_owned();
        let cleaned = clean_translated_text(strip_outer_quotes(&stripped));
        if cleaned.is_empty() {
            return false;
        }
        let lower = cleaned.to_ascii_lowercase();
        for bad_tok in [
            "items=",
            "'role'",
            "\"role\"",
            "'glossary'",
            "\"glossary\"",
            "'speaker'",
            "\"speaker\"",
        ] {
            if lower.contains(bad_tok) {
                return false;
            }
        }
        if reject_cjk_echo {
            let non_ws: Vec<char> = cleaned.chars().filter(|c| !c.is_whitespace()).collect();
            if !non_ws.is_empty() {
                let cjk_cnt = non_ws.iter().filter(|&&c| is_cjk_char(c)).count();
                if (cjk_cnt as f64) / (non_ws.len() as f64) > 0.35 {
                    return false;
                }
            }
        }
        *item = cleaned;
    }
    true
}

fn parse_google_response(val: &Value) -> Option<String> {
    if let Some(sentences) = val.get("sentences").and_then(|v| v.as_array()) {
        let joined: String = sentences
            .iter()
            .filter_map(|s| s.get("trans").and_then(|t| t.as_str()))
            .collect();
        let trimmed = joined.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(arr) = val.as_array() {
        if arr.iter().all(|v| v.is_string()) {
            let joined: String = arr.iter().filter_map(|v| v.as_str()).collect();
            let trimmed = joined.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        if let Some(s) = arr.first().and_then(|v| v.as_str()) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        if let Some(inner) = arr.first().and_then(|v| v.as_array()) {
            let mut joined = String::new();
            for seg in inner {
                if let Some(s) = seg.as_str() {
                    joined.push_str(s);
                } else if let Some(s) = seg.as_array().and_then(|a| a.first()).and_then(|v| v.as_str()) {
                    joined.push_str(s);
                }
            }
            let trimmed = joined.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub struct TranslatorService {
    cache: BoundedLruCache,
    agent: ureq::Agent,
    cache_path: PathBuf,
    pub engine: Engine,
    pub api_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

impl TranslatorService {
    pub fn new(
        engine: Engine,
        api_url: Option<String>,
        model: Option<String>,
        api_key: Option<String>,
    ) -> Self {
        let read_timeout_secs = match engine {
            Engine::Google => 4,
            Engine::OpenAi | Engine::Gemini => 8,
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2))
            .timeout_read(Duration::from_secs(read_timeout_secs))
            .timeout_write(Duration::from_secs(2))
            .build();
        Self {
            cache: BoundedLruCache::new(2048),
            agent,
            cache_path: default_cache_path(),
            engine,
            api_url,
            model,
            api_key,
        }
    }

    pub fn load_persistent_cache(&self) -> usize {
        let Ok(raw) = fs::read_to_string(&self.cache_path) else {
            return 0;
        };
        let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) else {
            return 0;
        };
        let mut count = 0usize;
        for (k, v) in map {
            if !v.is_empty() {
                self.cache.set(k, v);
                count += 1;
            }
        }
        count
    }

    pub fn save_persistent_cache(&self) {
        if let Some(parent) = self.cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let snapshot = self.cache.snapshot();
        let tmp = self.cache_path.with_extension(format!("tmp.{}", std::process::id()));
        if let Ok(json) = serde_json::to_string(&snapshot) {
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, &self.cache_path);
            }
        }
    }

    fn store_translation(
        &self,
        source_lang: &str,
        target_lang: &str,
        orig_txt: &str,
        trans_txt: &str,
        scene_sig: Option<&str>,
        ctx_key: Option<&str>,
    ) {
        let cleaned = orig_txt.trim();
        if cleaned.is_empty() || trans_txt.is_empty() {
            return;
        }
        let prefix = if self.engine == Engine::Google {
            String::new()
        } else {
            format!("{}:", self.engine.as_str())
        };
        if let Some(sig) = scene_sig {
            self.cache.set(
                format!("{prefix}{source_lang}:{target_lang}:ctx:{sig}:{cleaned}"),
                trans_txt.to_string(),
            );
        }
        if let Some(ck) = ctx_key {
            self.cache.set(
                format!("{prefix}{source_lang}:{target_lang}:turn:{ck}:{cleaned}"),
                trans_txt.to_string(),
            );
        }
        let norm = normalize_cache_text(cleaned);
        if prefix.is_empty() || norm.chars().count() > 12 {
            self.cache.set(
                format!("{prefix}{source_lang}:{target_lang}:{cleaned}"),
                trans_txt.to_string(),
            );
            if norm.chars().count() >= 4 {
                self.cache.set(
                    format!("{prefix}{source_lang}:{target_lang}:norm:{norm}"),
                    trans_txt.to_string(),
                );
            }
        }
    }

    fn lookup_cached(
        &self,
        orig_txt: &str,
        source_lang: &str,
        target_lang: &str,
        scene_sig: Option<&str>,
        ctx_key: Option<&str>,
    ) -> Option<String> {
        let cleaned = orig_txt.trim();
        if cleaned.is_empty() {
            return None;
        }
        let prefix = if self.engine == Engine::Google {
            String::new()
        } else {
            format!("{}:", self.engine.as_str())
        };
        if let Some(sig) = scene_sig {
            if let Some(hit) = self
                .cache
                .get(&format!("{prefix}{source_lang}:{target_lang}:ctx:{sig}:{cleaned}"))
            {
                return Some(hit);
            }
        }
        if let Some(ck) = ctx_key {
            if let Some(hit) = self
                .cache
                .get(&format!("{prefix}{source_lang}:{target_lang}:turn:{ck}:{cleaned}"))
            {
                return Some(hit);
            }
        }
        let norm = normalize_cache_text(cleaned);
        if !prefix.is_empty() && norm.chars().count() <= 12 {
            return None;
        }
        if let Some(hit) = self
            .cache
            .get(&format!("{prefix}{source_lang}:{target_lang}:{cleaned}"))
        {
            return Some(hit);
        }
        if norm.chars().count() >= 4 {
            return self
                .cache
                .get(&format!("{prefix}{source_lang}:{target_lang}:norm:{norm}"));
        }
        None
    }

    pub fn is_translation_cached(&self, text: &str, source_lang: &str, target_lang: &str) -> bool {
        self.lookup_cached(text, source_lang, target_lang, None, None)
            .is_some()
    }

    fn fast_google_request(&self, text: &str, source_lang: &str, target_lang: &str) -> Option<String> {
        // Primary: clients5 HTTPS
        let clients5 = self
            .agent
            .get("https://clients5.google.com/translate_a/t")
            .set("User-Agent", BROWSER_UA)
            .query("client", "dict-chrome-ex")
            .query("ie", "UTF-8")
            .query("oe", "UTF-8")
            .query("sl", source_lang)
            .query("tl", target_lang)
            .query("q", text)
            .call();

        if let Ok(resp) = clients5 {
            if let Ok(val) = resp.into_json::<Value>() {
                if let Some(parsed) = parse_google_response(&val) {
                    return Some(parsed);
                }
            }
        }

        // Failover: translate.googleapis.com HTTP
        let gtx = self
            .agent
            .get("http://translate.googleapis.com/translate_a/single")
            .set("User-Agent", BROWSER_UA)
            .query("client", "gtx")
            .query("ie", "UTF-8")
            .query("oe", "UTF-8")
            .query("dt", "t")
            .query("sl", source_lang)
            .query("tl", target_lang)
            .query("q", text)
            .call();

        if let Ok(resp) = gtx {
            if let Ok(val) = resp.into_json::<Value>() {
                return parse_google_response(&val);
            }
        }
        None
    }

    fn resolve_llm_config(&self) -> (String, String, String) {
        match self.engine {
            Engine::Gemini => {
                let model = self
                    .model
                    .clone()
                    .or_else(|| std::env::var("WAYTRANS_MODEL").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| "gemini-2.5-flash-lite".to_string());
                let key = self
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("WAYTRANS_API_KEY").ok().filter(|s| !s.is_empty()))
                    .or_else(|| std::env::var("GEMINI_API_KEY").ok().filter(|s| !s.is_empty()))
                    .or_else(|| std::env::var("GOOGLE_API_KEY").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_default();
                (String::new(), model, key)
            }
            _ => {
                let url = self
                    .api_url
                    .clone()
                    .or_else(|| std::env::var("WAYTRANS_API_URL").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| "http://127.0.0.1:11434/v1/chat/completions".to_string());
                let model = self
                    .model
                    .clone()
                    .or_else(|| std::env::var("WAYTRANS_MODEL").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| "qwen2.5:3b".to_string());
                let key = self
                    .api_key
                    .clone()
                    .or_else(|| std::env::var("WAYTRANS_API_KEY").ok().filter(|s| !s.is_empty()))
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()))
                    .or_else(|| std::env::var("GROQ_API_KEY").ok().filter(|s| !s.is_empty()))
                    .or_else(|| std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_default();
                (url, model, key)
            }
        }
    }

    fn request_llm(&self, system_prompt: &str, user_payload: &str) -> Option<String> {
        let (url, model, key) = self.resolve_llm_config();
        match self.engine {
            Engine::Gemini => {
                if key.is_empty() {
                    return None;
                }
                let endpoint = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
                );
                let body = serde_json::json!({
                    "system_instruction": { "parts": [{ "text": system_prompt }] },
                    "contents": [{ "role": "user", "parts": [{ "text": user_payload }] }],
                    "generationConfig": { "temperature": 0.2, "responseMimeType": "application/json" }
                });
                let resp = self
                    .agent
                    .post(&endpoint)
                    .query("key", &key)
                    .set("Content-Type", "application/json")
                    .send_json(body)
                    .ok()?;
                let val: Value = resp.into_json().ok()?;
                val.get("candidates")?
                    .get(0)?
                    .get("content")?
                    .get("parts")?
                    .get(0)?
                    .get("text")?
                    .as_str()
                    .map(|s| s.trim().to_string())
            }
            Engine::OpenAi => {
                let body = serde_json::json!({
                    "model": model,
                    "messages": [
                        { "role": "system", "content": system_prompt },
                        { "role": "user", "content": user_payload }
                    ],
                    "temperature": 0.2,
                    "response_format": { "type": "json_object" }
                });
                let mut req = self
                    .agent
                    .post(&url)
                    .set("Content-Type", "application/json");
                if !key.is_empty() {
                    req = req.set("Authorization", &format!("Bearer {key}"));
                }
                let resp = req.send_json(body).ok()?;
                let val: Value = resp.into_json().ok()?;
                val.get("choices")?
                    .get(0)?
                    .get("message")?
                    .get("content")?
                    .as_str()
                    .map(|s| s.trim().to_string())
            }
            Engine::Google => None,
        }
    }

    pub fn translate_blocks_llm(
        &self,
        metas: &[BlockMeta],
        history: &[DialogueTurn],
        source_lang: &str,
        target_lang: &str,
    ) -> Option<Vec<String>> {
        if metas.is_empty() {
            return Some(Vec::new());
        }
        let visible: Vec<&str> = metas.iter().map(|m| m.text.as_str()).collect();
        let filtered_hist: Vec<&DialogueTurn> = history
            .iter()
            .filter(|h| !visible.contains(&h.source.as_str()))
            .collect();
        let start = filtered_hist.len().saturating_sub(8);
        let recent_history = &filtered_hist[start..];

        let user_glossary = load_user_glossary(None);
        let mut corpus = String::new();
        for m in metas {
            corpus.push_str(&m.text);
            corpus.push(' ');
            corpus.push_str(&m.speaker);
            corpus.push(' ');
        }
        for h in recent_history {
            corpus.push_str(&h.source);
            corpus.push(' ');
            corpus.push_str(&h.speaker);
            corpus.push(' ');
        }

        let mut glossary: Vec<(&str, &str)> = Vec::new();
        if source_lang.to_ascii_lowercase().starts_with("ja")
            && target_lang.to_ascii_lowercase().starts_with("en")
        {
            for &(jp, en) in GAME_GLOSSARY_JA_EN {
                if corpus.contains(jp) {
                    glossary.push((jp, en));
                }
            }
        }
        for (k, v) in &user_glossary {
            if corpus.contains(k.as_str())
                || recent_history.iter().any(|h| &h.speaker == k)
            {
                glossary.push((k.as_str(), v.as_str()));
            }
        }

        let system_prompt = format!(
            "You translate numbered {source_lang} game lines into natural {target_lang}. \
             Use Glossary, Recent Dialogue, and any [Speaker] tag to resolve omitted pronouns (I, you, he, she, we, they). \
             Return ONLY valid JSON of the form {{\"translations\": [\"...\"]}} with exactly {} translated strings. \
             Never include {source_lang} text, JSON keys, or explanations inside the strings.",
            metas.len()
        );

        let mut user_prompt = String::new();
        if !glossary.is_empty() {
            user_prompt.push_str("Glossary:\n");
            for (k, v) in glossary {
                user_prompt.push_str(&format!("- {k} = {v}\n"));
            }
            user_prompt.push('\n');
        }
        if !recent_history.is_empty() {
            user_prompt.push_str("Recent Dialogue:\n");
            for h in recent_history {
                if h.speaker.is_empty() {
                    user_prompt.push_str(&format!("- {} => {}\n", h.source, h.translation));
                } else {
                    user_prompt.push_str(&format!(
                        "- [{}] {} => {}\n",
                        h.speaker, h.source, h.translation
                    ));
                }
            }
            user_prompt.push('\n');
        }
        user_prompt.push_str(&format!("Lines to translate ({}):\n", metas.len()));
        for (idx, m) in metas.iter().enumerate() {
            let clean_line = MULTI_NL_RE.replace_all(&m.text, " ");
            let mut prefix = String::new();
            if m.role == "choice" {
                prefix.push_str("[choice] ");
            }
            if !m.speaker.is_empty() {
                prefix.push_str(&format!("[{}] ", m.speaker));
            }
            user_prompt.push_str(&format!("{}. {}{}\n", idx + 1, prefix, clean_line));
        }

        let raw = self.request_llm(&system_prompt, &user_prompt)?;
        let mut parsed = parse_llm_json_array(&raw, metas.len())?;
        if !validate_llm_translations(&mut parsed, source_lang, target_lang) {
            return None;
        }
        Some(parsed)
    }

    pub fn translate_single(&self, text: &str, source_lang: &str, target_lang: &str) -> Result<String> {
        let cleaned = text.trim();
        if cleaned.is_empty() {
            return Ok(String::new());
        }
        if source_lang.eq_ignore_ascii_case(target_lang) {
            return Ok(cleaned.to_string());
        }

        if self.engine != Engine::Google {
            if let Some(hit) = self.lookup_cached(cleaned, source_lang, target_lang, None, None) {
                return Ok(hit);
            }
            let metas = [BlockMeta {
                index: 0,
                text: cleaned.to_string(),
                role: "dialogue",
                speaker: String::new(),
            }];
            if let Some(mut res) = self.translate_blocks_llm(&metas, &[], source_lang, target_lang) {
                if let Some(first) = res.pop() {
                    if !first.is_empty() {
                        self.store_translation(source_lang, target_lang, cleaned, &first, None, None);
                        return Ok(first);
                    }
                }
            }
        }

        if let Some(hit) = self.cache.get(&format!("{source_lang}:{target_lang}:{cleaned}")) {
            return Ok(hit);
        }
        let prepared = prepare_source_text(cleaned, source_lang, target_lang);
        if let Some(raw) = self.fast_google_request(&prepared, source_lang, target_lang) {
            let out = clean_translated_text(&raw);
            self.cache.set(format!("{source_lang}:{target_lang}:{cleaned}"), out.clone());
            return Ok(out);
        }
        Err(WaytransError::Translation("all translation backends failed".into()))
    }

    pub fn batch_translate_texts(
        &self,
        texts: &[String],
        source_lang: &str,
        target_lang: &str,
    ) -> Vec<String> {
        if texts.is_empty() {
            return Vec::new();
        }
        let mut results = vec![String::new(); texts.len()];
        let mut missing_indices = Vec::new();
        let mut missing_texts = Vec::new();

        for (i, t) in texts.iter().enumerate() {
            let cleaned = t.trim();
            if cleaned.is_empty() {
                continue;
            }
            if source_lang.eq_ignore_ascii_case(target_lang) {
                results[i] = cleaned.to_string();
                continue;
            }
            if let Some(hit) = self.cache.get(&format!("{source_lang}:{target_lang}:{cleaned}")) {
                results[i] = hit;
            } else {
                missing_indices.push(i);
                missing_texts.push(cleaned.to_string());
            }
        }

        if missing_texts.is_empty() {
            return results;
        }

        let prepared: Vec<String> = missing_texts
            .iter()
            .map(|t| prepare_source_text(t, source_lang, target_lang))
            .collect();
        let payload = prepared.join(" ||| ");
        if let Some(raw) = self.fast_google_request(&payload, source_lang, target_lang) {
            let parts: Vec<String> = PIPE_SPLIT_RE
                .split(&raw)
                .map(clean_translated_text)
                .collect();
            let matched = parts.len().min(missing_texts.len());
            for i in 0..matched {
                if !parts[i].is_empty() {
                    let idx = missing_indices[i];
                    results[idx] = parts[i].clone();
                    self.cache.set(
                        format!("{source_lang}:{target_lang}:{}", missing_texts[i]),
                        parts[i].clone(),
                    );
                }
            }
            if matched < missing_texts.len() {
                for i in matched..missing_texts.len() {
                    let p = prepare_source_text(&missing_texts[i], source_lang, target_lang);
                    if let Some(single) = self.fast_google_request(&p, source_lang, target_lang) {
                        let cleaned = clean_translated_text(&single);
                        results[missing_indices[i]] = cleaned.clone();
                        self.cache.set(
                            format!("{source_lang}:{target_lang}:{}", missing_texts[i]),
                            cleaned,
                        );
                    }
                }
            }
        } else if missing_texts.len() > 1 {
            for (idx, orig) in missing_indices.iter().zip(missing_texts.iter()) {
                let p = prepare_source_text(orig, source_lang, target_lang);
                if let Some(single) = self.fast_google_request(&p, source_lang, target_lang) {
                    let cleaned = clean_translated_text(&single);
                    results[*idx] = cleaned.clone();
                    self.cache
                        .set(format!("{source_lang}:{target_lang}:{orig}"), cleaned);
                }
            }
        }

        results
    }

    pub fn translate_contextual_blocks(
        &self,
        blocks: &[DialogueBlock],
        viewport_height: i32,
        dialogue_history: &[DialogueTurn],
        source_lang: &str,
        target_lang: &str,
    ) -> Vec<String> {
        if blocks.is_empty() {
            return Vec::new();
        }
        let metas = detect_block_roles_and_speakers(blocks, viewport_height);
        let scene_sig: String = metas
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join("|");

        if self.engine != Engine::Google {
            let last_turn_key = dialogue_history
                .last()
                .map(|t| format!("{}:{}", t.speaker, t.source))
                .unwrap_or_default();
            let mut llm_results = vec![String::new(); blocks.len()];
            let mut missing_llm = false;
            for (i, m) in metas.iter().enumerate() {
                if m.text.is_empty() {
                    continue;
                }
                let turn_ctx = format!("{}|{}", m.speaker, last_turn_key);
                if let Some(hit) = self.lookup_cached(
                    &m.text,
                    source_lang,
                    target_lang,
                    Some(&scene_sig),
                    Some(&turn_ctx),
                ) {
                    llm_results[i] = hit;
                } else {
                    missing_llm = true;
                }
            }
            if !missing_llm {
                return llm_results;
            }
            if let Some(out) = self.translate_blocks_llm(&metas, dialogue_history, source_lang, target_lang) {
                for (i, (m, tr)) in metas.iter().zip(out.into_iter()).enumerate() {
                    if !tr.is_empty() {
                        let turn_ctx = format!("{}|{}", m.speaker, last_turn_key);
                        self.store_translation(
                            source_lang,
                            target_lang,
                            &m.text,
                            &tr,
                            Some(&scene_sig),
                            Some(&turn_ctx),
                        );
                        llm_results[i] = tr;
                    }
                }
                return llm_results;
            }
            debug!("LLM translation unavailable/failed; falling back to Google Translate");
        }

        if !source_lang.to_ascii_lowercase().starts_with("ja") {
            let texts: Vec<String> = blocks.iter().map(|b| b.combined_text.clone()).collect();
            return self.batch_translate_texts(&texts, source_lang, target_lang);
        }

        let bottom_threshold = if viewport_height > 200 {
            (viewport_height as f64 * 0.64) as i32
        } else {
            650
        };
        let mut prompt_indices = Vec::new();
        let mut choice_indices = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            if b.top >= bottom_threshold || b.width() > 580 {
                prompt_indices.push(i);
            } else {
                choice_indices.push(i);
            }
        }
        let has_choices_and_prompt = !prompt_indices.is_empty() && !choice_indices.is_empty();

        let mut results = vec![String::new(); blocks.len()];
        let mut missing = false;
        for (i, b) in blocks.iter().enumerate() {
            let txt = b.combined_text.trim();
            if txt.is_empty() {
                continue;
            }
            if let Some(hit) = self.lookup_cached(txt, source_lang, target_lang, Some(&scene_sig), None) {
                results[i] = hit;
            } else {
                missing = true;
            }
        }
        if !missing {
            return results;
        }

        let collapse_nl = |s: &str| -> String {
            if source_lang.to_ascii_lowercase().starts_with("ja")
                || source_lang.to_ascii_lowercase().starts_with("zh")
                || source_lang.to_ascii_lowercase().starts_with("ko")
            {
                MULTI_NL_RE.replace_all(s, "").into_owned()
            } else {
                MULTI_NL_RE.replace_all(s, " ").into_owned()
            }
        };

        let visible: Vec<&str> = blocks.iter().map(|b| b.combined_text.trim()).collect();
        let hist_candidates: Vec<String> = dialogue_history
            .iter()
            .map(|h| h.source.trim().to_string())
            .filter(|s| !s.is_empty() && !visible.contains(&s.as_str()))
            .collect();
        let hist_start = hist_candidates.len().saturating_sub(2);
        let hist_tail = &hist_candidates[hist_start..];

        let mut script_lines = Vec::new();
        let mut line_to_block_idx: Vec<Option<usize>> = Vec::new();
        for (idx, h_text) in hist_tail.iter().enumerate() {
            let c = collapse_nl(h_text);
            script_lines.push(format!(
                "文脈{}：{}",
                idx + 1,
                prepare_source_text(&c, source_lang, target_lang)
            ));
            line_to_block_idx.push(None);
        }

        if has_choices_and_prompt {
            for (p_num, &blk_i) in prompt_indices.iter().enumerate() {
                let raw_t = prepare_source_text(
                    &collapse_nl(blocks[blk_i].combined_text.trim()),
                    source_lang,
                    target_lang,
                );
                let label = if prompt_indices.len() == 1 {
                    "質問".to_string()
                } else {
                    format!("質問{}", p_num + 1)
                };
                script_lines.push(format!("{label}：{raw_t}"));
                line_to_block_idx.push(Some(blk_i));
            }
            for (c_num, &blk_i) in choice_indices.iter().enumerate() {
                let raw_t = prepare_source_text(
                    &collapse_nl(blocks[blk_i].combined_text.trim()),
                    source_lang,
                    target_lang,
                );
                script_lines.push(format!("選択肢{}：{raw_t}", c_num + 1));
                line_to_block_idx.push(Some(blk_i));
            }
        } else if blocks.len() == 1 && hist_tail.is_empty() {
            let raw_t = prepare_source_text(blocks[0].combined_text.trim(), source_lang, target_lang);
            script_lines.push(raw_t);
            line_to_block_idx.push(Some(0));
        } else {
            for (d_num, b) in blocks.iter().enumerate() {
                let raw_t = prepare_source_text(
                    &collapse_nl(b.combined_text.trim()),
                    source_lang,
                    target_lang,
                );
                script_lines.push(format!("台詞{}：{raw_t}", d_num + 1));
                line_to_block_idx.push(Some(d_num));
            }
        }

        let payload = script_lines.join("\n");
        let raw_out = self.fast_google_request(&payload, source_lang, target_lang);
        if let Some(ref raw) = raw_out {
            if blocks.len() == 1 && hist_tail.is_empty() && !has_choices_and_prompt {
                let cleaned = clean_translated_text(strip_outer_quotes(raw));
                let orig = blocks[0].combined_text.trim();
                results[0] = cleaned.clone();
                self.store_translation(
                    source_lang,
                    target_lang,
                    orig,
                    &cleaned,
                    Some(&scene_sig),
                    None,
                );
                return results;
            }

            let out_lines: Vec<&str> = raw.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            if out_lines.len() == script_lines.len() {
                for (blk_opt, out_line) in line_to_block_idx.iter().zip(out_lines.iter()) {
                    let Some(blk_i) = blk_opt else {
                        continue;
                    };
                    let stripped_prefix = CONTEXT_PREFIX_RE.replace(out_line, "");
                    let cleaned = clean_translated_text(strip_outer_quotes(stripped_prefix.as_ref()));
                    results[*blk_i] = cleaned.clone();
                    let orig = blocks[*blk_i].combined_text.trim();
                    self.store_translation(
                        source_lang,
                        target_lang,
                        orig,
                        &cleaned,
                        Some(&scene_sig),
                        None,
                    );
                }
                return results;
            }
        }

        let missing_idxs: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(i, r)| r.is_empty() && !blocks[*i].combined_text.trim().is_empty())
            .map(|(i, _)| i)
            .collect();
        if !missing_idxs.is_empty() {
            let texts: Vec<String> = missing_idxs
                .iter()
                .map(|&i| blocks[i].combined_text.clone())
                .collect();
            let fallback = self.batch_translate_texts(&texts, source_lang, target_lang);
            for (idx, tr) in missing_idxs.into_iter().zip(fallback.into_iter()) {
                results[idx] = tr;
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_llm_json_variants() {
        assert_eq!(
            parse_llm_json_array(r#"["Hello", "World"]"#, 2).unwrap(),
            vec!["Hello", "World"]
        );
        assert_eq!(
            parse_llm_json_array("```json\n[\"He left.\", \"Did she?\"]\n```", 2).unwrap(),
            vec!["He left.", "Did she?"]
        );
        assert_eq!(
            parse_llm_json_array(r#"{"translations": ["One"]}"#, 1).unwrap(),
            vec!["One"]
        );
        assert!(parse_llm_json_array(r#"["Only one"]"#, 2).is_none());
    }

    #[test]
    fn detects_speaker_and_attaches_to_dialogue() {
        let blocks = vec![
            DialogueBlock {
                lines: Vec::new(),
                left: 80,
                top: 700,
                right: 280,
                bottom: 745,
                combined_text: "花村陽介".into(),
                avg_line_height: 45,
            },
            DialogueBlock {
                lines: Vec::new(),
                left: 85,
                top: 770,
                right: 680,
                bottom: 825,
                combined_text: "明日、ジュネスに来いよ".into(),
                avg_line_height: 55,
            },
        ];
        let metas = detect_block_roles_and_speakers(&blocks, 1080);
        assert_eq!(metas[0].role, "speaker");
        assert_eq!(metas[1].speaker, "花村陽介");
    }

    #[test]
    fn hesitant_question_and_glossary() {
        let p = prepare_source_text("マヨナカテレビを見てみようか・・・", "ja", "en");
        assert_eq!(p, "the Midnight Channelを見てみようか？...");
        assert_eq!(
            clean_translated_text("Shall we watch the the Midnight Channel? ..."),
            "Shall we watch the Midnight Channel?..."
        );
    }

    #[test]
    fn rejects_leaked_metadata_and_untranslated_cjk_echoes() {
        let mut leaked = vec!["items=[['role','dialogue','text','マヨナカテレビ']]".to_string()];
        assert!(!validate_llm_translations(&mut leaked, "ja", "en"));

        let mut cjk_echo = vec!["マヨナカテレビを見てみようか".to_string()];
        assert!(!validate_llm_translations(&mut cjk_echo, "ja", "en"));

        let mut ok = vec!["1. [Yosuke] \"Should we watch the Midnight Channel?...\"".to_string()];
        assert!(validate_llm_translations(&mut ok, "ja", "en"));
        assert_eq!(ok[0], "Should we watch the Midnight Channel?...");
    }
}
