use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::types::{DialogueBlock, OcrLine, OverlayItem, OverlayStyle, Rect};

static DATE_CLOCK_HUD_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:\d{1,2}\s*[/:]\s*\d{1,2}|\d{3,4}[日月火水木金土]?)(?:\s*[日月火水木金土昼朝夕夜]{0,2})?$")
        .expect("valid regex")
});
static DIGIT_COUNTER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\d+\s*[人匹枚回個本台件分秒時間日年月階歳円万億%％位番話章]")
        .expect("valid regex")
});
static INVALID_YOON_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([ァ-ヶー][ゃゅょ]|[ぁ-んー][ャュョ])").expect("valid regex")
});
static LEADING_CURSOR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[>▶▷•・\s]+").expect("valid regex"));
static STRIP_CORE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[…・.。、!?！？「」『』()（）\s]+").expect("valid regex"));
static PURE_KATA_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[ァ-ヶー]+$").expect("valid regex"));
static HIRA_PLUS_KATA_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[ぁ-ん][ァ-ヶー]{3,}$").expect("valid regex"));
static KANA_DIGIT_GLITCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[ぁ-んァ-ヶ]\d|\d[ぁ-んァ-ヶ]").expect("valid regex"));

const INVALID_LEADING_KANA: &str = "ンんーヽヾゝゞぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮ";
const LINE_NOISE_SET: &str =
    "一二三ノハニトリロ口十丨｜ー・エユコヨレしンソツシテクフヘベペくへビピっッにヽヾミメワウイカ力夕丁工川丿丶々";
const HALLUCINATION_SET: &str =
    "一二三ノハニトリロ口十丨｜ー・エユコヨレしンソツフヘベペくへビピっッてでにはをかのやゃホボポきぎさざただなまらりるれろ";

const VALID_TWO_CHAR_HIRAGANA: &[&str] = &[
    "はい", "いい", "まだ", "なに", "どこ", "だれ", "なぜ", "そう", "もう", "よし", "えっ", "あっ",
    "うん", "うそ", "だめ", "いや", "でも", "ほら", "ただ", "これ", "それ", "あれ", "どれ", "ここ",
    "そこ", "あの", "その", "また", "いま", "すぐ", "いつ", "かも", "まあ", "おや", "あら", "ねえ",
    "ほお",
];

const FRAGMENT_ONLY_SET: &[&str] = &[
    "って", "った", "ない", "ます", "した", "です", "から", "けど", "のに", "ので", "らない",
    "てみる", "ました", "ません", "でした", "ている", "てきた", "だろう",
];

#[inline]
pub fn is_cjk_char(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{309f}' | '\u{30a0}'..='\u{30ff}' | '\u{4e00}'..='\u{9faf}')
}

#[inline]
pub fn is_kanji_char(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9faf}')
}

#[inline]
pub fn is_cjk_keep_token(c: char) -> bool {
    is_cjk_char(c)
        || matches!(
            c,
            '\u{3001}'..='\u{303f}'
                | '\u{ff01}'..='\u{ff60}'
                | '!'
                | '?'
                | '0'..='9'
        )
}

/// Filter out OCR noise, 3D background wire hallucinations, UI date stamps, and incomplete fragments.
pub fn is_valid_japanese_dialogue(text: &str, require_complete: bool) -> bool {
    let cleaned = text.trim();
    if cleaned.is_empty() {
        return false;
    }

    let body_cow = LEADING_CURSOR_RE.replace(cleaned, "");
    let body = body_cow.as_ref();
    if body.is_empty() {
        return false;
    }

    if DATE_CLOCK_HUD_RE.is_match(body) {
        return false;
    }
    if body.chars().any(|c| matches!(c, '[' | ']' | '{' | '}' | '|' | '<' | '>' | '【' | '】')) {
        return false;
    }
    if body.matches('(').count() != body.matches(')').count()
        || body.matches('（').count() != body.matches('）').count()
    {
        return false;
    }

    let cjk_chars: Vec<char> = body.chars().filter(|&c| is_cjk_char(c)).collect();
    let cjk_count = cjk_chars.len();
    if cjk_count == 0 {
        return false;
    }

    let digit_count = body.chars().filter(|c| c.is_ascii_digit()).count();
    if digit_count > 0 && !DIGIT_COUNTER_RE.is_match(body) {
        if cjk_count <= 4 || KANA_DIGIT_GLITCH_RE.is_match(body) {
            return false;
        }
    }

    let latin_count = body.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    if latin_count >= cjk_count && cjk_count < 3 {
        return false;
    }
    if (latin_count as f64) > (cjk_count as f64) * 1.5 {
        return false;
    }

    let core_cow = STRIP_CORE_RE.replace_all(body, "");
    let core = core_cow.as_ref();
    let Some(first_core) = core.chars().next() else {
        return false;
    };
    if INVALID_LEADING_KANA.contains(first_core) {
        return false;
    }
    if INVALID_YOON_RE.is_match(core) {
        return false;
    }

    let cjk_core: String = cjk_chars.iter().collect();
    let kanji_count = cjk_chars.iter().filter(|&&c| is_kanji_char(c)).count();
    let has_sentence_punct = body.chars().any(|c| matches!(c, '？' | '?' | '！' | '!'));

    if kanji_count == 0 {
        let unique: HashSet<char> = cjk_chars.iter().copied().collect();
        if cjk_chars.len() >= 3 && unique.len() == 1 {
            return false;
        }

        let stroke_chars = cjk_chars.iter().filter(|&&c| LINE_NOISE_SET.contains(c)).count();
        if stroke_chars == cjk_chars.len() {
            return false;
        }

        if cjk_chars.len() >= 3 {
            let mut best_char = cjk_chars[0];
            let mut max_freq = 0usize;
            for &ch in &unique {
                let cnt = cjk_chars.iter().filter(|&&x| x == ch).count();
                if cnt > max_freq {
                    max_freq = cnt;
                    best_char = ch;
                }
            }
            if (stroke_chars as f64) / (cjk_chars.len() as f64) >= 0.75
                && (max_freq >= 3 || stroke_chars >= 4)
            {
                return false;
            }
            if LINE_NOISE_SET.contains(best_char)
                && max_freq >= 3
                && (max_freq as f64) / (cjk_chars.len() as f64) >= 0.45
            {
                return false;
            }
            if cjk_chars.len() >= 4 && PURE_KATA_RE.is_match(&cjk_core) && unique.len() <= 2 {
                return false;
            }
        }

        if cjk_chars.len() <= 6
            && HIRA_PLUS_KATA_RE.is_match(&cjk_core)
            && !cjk_core.starts_with('お')
        {
            return false;
        }
    }

    if cjk_chars.len() == 1 {
        if HALLUCINATION_SET.contains(cjk_chars[0]) || kanji_count == 0 {
            return false;
        }
        if require_complete && !has_sentence_punct {
            return false;
        }
    }

    if (2..=3).contains(&cjk_chars.len()) && kanji_count == 0 {
        let is_valid_two = VALID_TWO_CHAR_HIRAGANA.contains(&cjk_core.as_str());
        if require_complete
            && !is_valid_two
            && cjk_chars
                .first()
                .map(|&c| matches!(c, 'た' | 'て' | 'だ' | 'で' | 'ぜ' | 'よ' | 'わ' | 'ね'))
                .unwrap_or(false)
        {
            return false;
        }
    }

    if cjk_chars.len() == 2 && kanji_count == 0 {
        if PURE_KATA_RE.is_match(&cjk_core) && !has_sentence_punct {
            return false;
        }
        let is_valid_two = VALID_TWO_CHAR_HIRAGANA.contains(&cjk_core.as_str());
        if cjk_chars.iter().all(|&c| HALLUCINATION_SET.contains(c)) && !is_valid_two {
            return false;
        }
        if require_complete && !has_sentence_punct && !is_valid_two {
            return false;
        }
    }

    if require_complete && FRAGMENT_ONLY_SET.contains(&cjk_core.as_str()) && !has_sentence_punct {
        return false;
    }

    let prolonged = body.matches('ー').count();
    if prolonged >= 2 && body.chars().count().saturating_sub(prolonged) <= 1 {
        return false;
    }
    if body.matches('ロ').count() >= 2 {
        return false;
    }

    true
}

/// Validate OCR lines for non-Japanese source languages.
pub fn is_valid_text_line(text: &str, source_lang: &str) -> bool {
    let sl = source_lang.to_ascii_lowercase();
    if matches!(sl.as_str(), "ja" | "jp" | "jpn") {
        return is_valid_japanese_dialogue(text, true);
    }
    let cleaned = text.trim();
    if cleaned.is_empty() {
        return false;
    }
    let alnum: Vec<char> = cleaned
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    if alnum.len() < 2 {
        return false;
    }
    let non_space = cleaned.chars().filter(|c| !c.is_whitespace()).count();
    if non_space > 0 && (alnum.len() as f64 / non_space as f64) < 0.5 {
        return false;
    }
    if alnum.len() >= 3 {
        let unique: HashSet<char> = alnum
            .iter()
            .flat_map(|c| c.to_lowercase())
            .collect();
        if unique.len() == 1 {
            return false;
        }
    }
    true
}

fn cluster_bounds(cluster: &[OcrLine]) -> (i32, i32, i32, i32, i32, String) {
    let min_left = cluster.iter().map(|l| l.left).min().unwrap_or(0);
    let min_top = cluster.iter().map(|l| l.top).min().unwrap_or(0);
    let max_right = cluster.iter().map(|l| l.right()).max().unwrap_or(0);
    let max_bottom = cluster.iter().map(|l| l.bottom()).max().unwrap_or(0);
    let avg_h = (cluster.iter().map(|l| l.height).sum::<i32>() / cluster.len().max(1) as i32).max(1);
    let mut ordered = cluster.to_vec();
    ordered.sort_by_key(|l| l.left);
    let combined: String = ordered.into_iter().map(|l| l.text).collect();
    (min_left, min_top, max_right, max_bottom, avg_h, combined)
}

fn can_merge_paragraph_rows(upper: &[OcrLine], lower: &[OcrLine]) -> bool {
    let (u_left, u_top, u_right, u_bottom, u_h, u_text) = cluster_bounds(upper);
    let (l_left, l_top, l_right, l_bottom, l_h, _l_text) = cluster_bounds(lower);

    let avg_h = (u_h as f64 + l_h as f64) / 2.0;
    let gap = l_top - u_bottom;
    let u_cy = (u_top as f64 + u_bottom as f64) / 2.0;
    let l_cy = (l_top as f64 + l_bottom as f64) / 2.0;
    if (l_cy - u_cy).abs() <= avg_h * 0.55 {
        return false;
    }
    let min_gap = -((avg_h * 0.20) as i32).max(4);
    if gap < min_gap || (gap as f64) > avg_h * 0.55 {
        return false;
    }

    let height_ratio = u_h as f64 / l_h.max(1) as f64;
    if !(0.75..=1.33).contains(&height_ratio) {
        return false;
    }

    if u_right.min(l_right) <= u_left.max(l_left) {
        return false;
    }
    if (u_left - l_left).abs() > ((avg_h * 1.4) as i32).max(36) {
        return false;
    }

    let u_chars = u_text.chars().count();
    let u_last = match u_text.chars().last() {
        Some(c) => c,
        None => return false,
    };

    let u_width = u_right - u_left;
    if u_chars < 6 && (u_width as f64) < avg_h * 5.0 {
        return false;
    }
    if matches!(u_last, '。' | '！' | '？' | '!' | '?') {
        return false;
    }
    if u_chars <= 10 && !matches!(u_last, '、' | ',') {
        return false;
    }

    true
}

/// Cluster horizontal fragments on the same row and merge tightly-stacked wrapped paragraph lines.
pub fn cluster_text_lines(raw_lines: &[OcrLine]) -> Vec<DialogueBlock> {
    let mut lines: Vec<OcrLine> = raw_lines
        .iter()
        .filter(|l| !l.text.trim().is_empty())
        .cloned()
        .map(|mut l| {
            l.text = l.text.trim().to_string();
            l
        })
        .collect();

    if lines.is_empty() {
        return Vec::new();
    }

    lines.sort_by_key(|l| (l.top, l.left));

    let max_horizontal_gap = 85;
    let mut row_clusters: Vec<Vec<OcrLine>> = Vec::new();

    for line in lines {
        let line_cy = line.top as f64 + (line.height as f64 / 2.0);
        let mut assigned = false;
        for cluster in &mut row_clusters {
            for member in cluster.iter() {
                let member_cy = member.top as f64 + (member.height as f64 / 2.0);
                let vert_overlap = line.bottom().min(member.bottom()) - line.top.max(member.top);
                let horiz_dist = (line.left - member.right())
                    .max(member.left - line.right())
                    .max(0);
                let avg_h = (line.height as f64 + member.height as f64) / 2.0;
                let horiz_cap = if avg_h >= 28.0 {
                    ((avg_h * 3.5) as i32).min(240)
                } else {
                    max_horizontal_gap.min(((avg_h * 1.6) as i32).max(18))
                };

                if (line_cy - member_cy).abs() <= avg_h * 0.45
                    && (vert_overlap as f64) > avg_h * 0.35
                    && horiz_dist <= horiz_cap
                {
                    cluster.push(line.clone());
                    assigned = true;
                    break;
                }
            }
            if assigned {
                break;
            }
        }
        if !assigned {
            row_clusters.push(vec![line]);
        }
    }

    row_clusters.sort_by_key(|c| {
        (
            c.iter().map(|l| l.top).min().unwrap_or(0),
            c.iter().map(|l| l.left).min().unwrap_or(0),
        )
    });

    let mut merged_paragraphs: Vec<Vec<Vec<OcrLine>>> = Vec::new();
    for mut row in row_clusters {
        row.sort_by_key(|l| l.left);
        let should_merge = merged_paragraphs
            .last()
            .and_then(|p| p.last())
            .map(|prev| can_merge_paragraph_rows(prev, &row))
            .unwrap_or(false);
        if should_merge {
            if let Some(last) = merged_paragraphs.last_mut() {
                last.push(row);
            }
        } else {
            merged_paragraphs.push(vec![row]);
        }
    }

    let mut blocks = Vec::with_capacity(merged_paragraphs.len());
    for rows in merged_paragraphs {
        let row_strings: Vec<String> = rows
            .iter()
            .map(|row| row.iter().map(|l| l.text.as_str()).collect::<String>())
            .collect();
        let combined_text = row_strings.join("\n");
        let flat_lines: Vec<OcrLine> = rows.into_iter().flatten().collect();
        let min_left = flat_lines.iter().map(|l| l.left).min().unwrap_or(0);
        let min_top = flat_lines.iter().map(|l| l.top).min().unwrap_or(0);
        let max_right = flat_lines.iter().map(|l| l.right()).max().unwrap_or(0);
        let max_bottom = flat_lines.iter().map(|l| l.bottom()).max().unwrap_or(0);
        let avg_h =
            flat_lines.iter().map(|l| l.height).sum::<i32>() / flat_lines.len().max(1) as i32;

        blocks.push(DialogueBlock {
            lines: flat_lines,
            left: min_left,
            top: min_top,
            right: max_right,
            bottom: max_bottom,
            combined_text,
            avg_line_height: avg_h.max(16),
        });
    }

    blocks
}

/// Compute initial subtitle placement `(pos_x, pos_y, font_size, max_w, est_height)`.
pub fn compute_smart_placement(
    block: &DialogueBlock,
    translated_text: &str,
    viewport_width: i32,
    viewport_height: i32,
    scan_offset: (i32, i32),
    style: OverlayStyle,
) -> (i32, i32, i32, i32, i32) {
    let font_size = match style {
        OverlayStyle::Inplace => ((block.avg_line_height as f64 * 0.44).round() as i32).clamp(11, 26),
        OverlayStyle::Above => ((block.avg_line_height as f64 * 0.65) as i32).clamp(13, 18),
    };
    let estimated_char_w = font_size as f64 * 0.68;
    let text_len = translated_text.chars().count().max(1) as f64;
    let natural_w = (text_len * estimated_char_w) as i32 + 18;

    let abs_left = scan_offset.0 + block.left;
    let abs_top = scan_offset.1 + block.top;
    let abs_bottom = scan_offset.1 + block.bottom;

    if style == OverlayStyle::Inplace {
        let col_cap = (block.width() + 90).max((block.width() as f64 * 1.55) as i32);
        let max_w = (viewport_width - 20).min(60.max(col_cap.min(block.width().max(natural_w))));
        let lines_est = ((natural_w - 1) / max_w.max(1)) + 1;
        let est_h = ((lines_est as f64 * font_size as f64 * 1.28) as i32 + 8).max(block.height());
        let pos_x = abs_left.clamp(8, (viewport_width - max_w - 8).max(8));
        let pos_y = abs_top.clamp(8, (viewport_height - est_h - 8).max(8));
        return (pos_x, pos_y, font_size, max_w, est_h);
    }

    let single_line_w = (natural_w + 16).max(block.width());
    let max_w = single_line_w.max(120).min((viewport_width - 20).max(120));
    let lines_est = ((natural_w - 1) / max_w.max(1)) + 1;
    let est_h = (lines_est as f64 * font_size as f64 * 1.24) as i32 + 3;
    let pos_x = abs_left.clamp(8, (viewport_width - max_w - 8).max(8));

    let margin = 5;
    let mut target_y = abs_top - est_h - margin;
    if target_y < 8 {
        target_y = abs_bottom + margin;
    }
    let pos_y = target_y.clamp(8, (viewport_height - est_h - 8).max(8));
    (pos_x, pos_y, font_size, max_w, est_h)
}

/// Horizontal wrap ceiling for an `inplace` pill on multi-column pages.
pub fn compute_inplace_wrap_width(
    item: &OverlayItem,
    avail_w: i32,
    items: &[OverlayItem],
    source_obstacles: &[Rect],
) -> i32 {
    let pos_x = item.pos_x;
    let src_left = item.src_left_mon;
    let src_w = item.src_w;
    let src_top = item.src_top_mon;
    let src_bottom = item.src_bottom_mon;

    let mut limit = avail_w.max(80);
    if src_w > 0 {
        limit = limit.min(80.max(src_w + 90).max((src_w as f64 * 1.55) as i32));
    }

    let right_edge_min = if src_w > 0 {
        src_left + ((src_w as f64 * 0.85) as i32).max(30)
    } else {
        pos_x + 40
    };

    let mut candidates: Vec<Rect> = source_obstacles.to_vec();
    for other in items {
        if std::ptr::eq(other, item)
            || (other.src_left_mon == item.src_left_mon
                && other.src_top_mon == item.src_top_mon
                && other.src_bottom_mon == item.src_bottom_mon
                && other.src_w == item.src_w)
        {
            continue;
        }
        let ow = if other.src_w > 0 {
            other.src_w
        } else {
            other.max_width
        };
        let oh = (other.src_bottom_mon - other.src_top_mon).max(1);
        candidates.push(Rect::from_xywh(other.src_left_mon, other.src_top_mon, ow, oh));
    }

    for obs in candidates {
        if obs.width() <= 0 || obs.height() <= 0 {
            continue;
        }
        if obs.top < src_bottom + 4 && obs.bottom > src_top - 4 && obs.left >= right_edge_min {
            limit = limit.min((obs.left - pos_x - 8).max(80));
        }
    }

    limit.max(80)
}

#[inline]
fn rects_overlap_xywh(ax: i32, ay: i32, aw: i32, ah: i32, bx: i32, by: i32, bw: i32, bh: i32) -> bool {
    ax < (bx + bw) && bx < (ax + aw) && ay < (by + bh) && by < (ay + ah)
}

fn walk_clear_y<F>(
    start_y: i32,
    h1: i32,
    direction: i32,
    max_y: i32,
    hit_fn: F,
) -> Option<i32>
where
    F: Fn(i32) -> Option<(i32, i32, i32, i32)>,
{
    let mut y_try = start_y.clamp(4, max_y);
    for _ in 0..16 {
        let Some(hit) = hit_fn(y_try) else {
            return Some(y_try);
        };
        if direction < 0 {
            let next_y = hit.1 - h1 - 3;
            if next_y < 4 || next_y >= y_try {
                return None;
            }
            y_try = next_y;
        } else {
            let next_y = hit.1 + hit.3 + 3;
            if next_y > max_y || next_y <= y_try {
                return None;
            }
            y_try = next_y;
        }
    }
    if hit_fn(y_try).is_none() {
        Some(y_try)
    } else {
        None
    }
}

/// Nudge overlapping subtitle items vertically so they never overlap each other or source boxes.
pub fn resolve_subtitle_collisions(
    items: &mut [OverlayItem],
    viewport_height: i32,
    source_obstacles: &[Rect],
    prefer_down: bool,
) {
    if items.is_empty() || (items.len() < 2 && source_obstacles.is_empty()) {
        return;
    }

    let mut order: Vec<usize> = (0..items.len()).collect();
    if prefer_down {
        order.sort_by_key(|&idx| items[idx].pos_y);
    } else {
        order.sort_by(|&a, &b| items[b].pos_y.cmp(&items[a].pos_y));
    }

    let mut placed: Vec<(usize, i32, i32, i32, i32)> = Vec::new();
    let padded_sources: Vec<(i32, i32, i32, i32)> = source_obstacles
        .iter()
        .filter(|r| r.width() > 0 && r.height() > 0)
        .map(|r| (r.left, (r.top - 3).max(0), r.width(), r.height() + 6))
        .collect();

    for idx in order {
        let orig_y = items[idx].pos_y;
        let x1 = items[idx].pos_x;
        let w1 = if items[idx].actual_w > 0 {
            items[idx].actual_w
        } else {
            items[idx].max_width
        };
        let h1 = items[idx].est_height;
        let max_y = (viewport_height - h1 - 4).max(4);
        let mut y1 = orig_y.clamp(4, max_y);

        let first_placed_hit = |cy: i32, placed_vec: &[(usize, i32, i32, i32, i32)]| -> Option<(i32, i32, i32, i32)> {
            placed_vec
                .iter()
                .copied()
                .find(|&(_, bx, by, bw, bh)| rects_overlap_xywh(x1, cy, w1, h1, bx, by, bw, bh))
                .map(|(_, bx, by, bw, bh)| (bx, by, bw, bh))
        };
        let first_source_hit = |cy: i32| -> Option<(i32, i32, i32, i32)> {
            padded_sources
                .iter()
                .copied()
                .find(|&(bx, by, bw, bh)| rects_overlap_xywh(x1, cy, w1, h1, bx, by, bw, bh))
        };

        if prefer_down {
            let overlapping_placed: Vec<(usize, i32)> = placed
                .iter()
                .filter(|&&(_, bx, by, bw, bh)| rects_overlap_xywh(x1, y1, w1, h1, bx, by, bw, bh))
                .map(|&(p_idx, _, by, _, _)| (p_idx, by))
                .collect();

            if !overlapping_placed.is_empty()
                && overlapping_placed
                    .iter()
                    .all(|&(_, by)| by < y1 && (y1 - by) >= 26)
            {
                for &(p_idx, by) in &overlapping_placed {
                    let trimmed_h = (y1 - by - 2).max(24);
                    if trimmed_h < items[p_idx].est_height {
                        items[p_idx].est_height = trimmed_h;
                        for entry in placed.iter_mut() {
                            if entry.0 == p_idx {
                                entry.4 = trimmed_h;
                            }
                        }
                    }
                }
            }
        }

        let first_any_hit = |cy: i32| -> Option<(i32, i32, i32, i32)> {
            first_source_hit(cy).or_else(|| first_placed_hit(cy, &placed))
        };

        let src_hit = first_source_hit(y1);
        let placed_hit = first_placed_hit(y1, &placed);

        if src_hit.is_some() || placed_hit.is_some() {
            let mut chosen = if prefer_down {
                walk_clear_y(y1, h1, 1, max_y, first_any_hit)
                    .or_else(|| walk_clear_y(y1, h1, -1, max_y, first_any_hit))
            } else {
                let mut c = walk_clear_y(y1, h1, -1, max_y, first_any_hit);
                if c.is_none() {
                    let cand_below = items[idx].src_bottom_mon + 5;
                    if cand_below <= max_y && first_any_hit(cand_below).is_none() {
                        c = Some(cand_below);
                    }
                }
                c.or_else(|| walk_clear_y(y1, h1, 1, max_y, first_any_hit))
            };

            let mut candidate_y = y1;
            if let Some(cy) = chosen.take() {
                candidate_y = cy;
            } else if let Some(ph) = placed_hit {
                if prefer_down {
                    let cand_down = ph.1 + ph.3 + 2;
                    candidate_y = if cand_down <= max_y {
                        cand_down
                    } else {
                        (ph.1 - h1 - 2).max(4)
                    };
                } else {
                    let cand_up = ph.1 - h1 - 2;
                    candidate_y = if cand_up >= 4 {
                        cand_up
                    } else {
                        (ph.1 + ph.3 + 2).min(max_y)
                    };
                }
            }

            if prefer_down {
                if (candidate_y - orig_y).abs() <= 16 {
                    y1 = candidate_y;
                }
            } else {
                y1 = candidate_y;
            }
        }

        let final_y = y1.clamp(4, max_y);
        items[idx].pos_y = final_y;
        placed.push((idx, x1, final_y, w1, items[idx].est_height));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wire_gibberish_and_accepts_valid_dialogue() {
        for bad in [
            "にニニンーーンンー",
            "ンンンンンン",
            "シン",
            "ーーーーー",
            "04/16 土 夜",
            "らない",
            "デーデー",
            "あああ",
        ] {
            assert!(!is_valid_japanese_dialogue(bad, true), "should reject {bad}");
        }
        for ok in [
            "部屋で見てみる",
            "まだ戻らない",
            "マヨナカテレビを見てみようか…",
            "> マヨナカテレビを見てみようか・・・",
            "花村 陽介",
            "明日は日曜だし、",
            "朝イチで、ジュネスに集合な!",
            "はい",
            "いいえ",
            "10分後に行こう",
        ] {
            assert!(is_valid_japanese_dialogue(ok, true), "should accept {ok}");
        }
    }

    #[test]
    fn clusters_paragraphs_but_never_menu_choices_or_speaker() {
        let lines = vec![
            OcrLine { text: "花村陽介".into(), left: 100, top: 720, width: 130, height: 34, conf: 90.0 },
            OcrLine { text: "明日は日曜だし、".into(), left: 100, top: 770, width: 360, height: 36, conf: 90.0 },
            OcrLine { text: "朝イチでジュネスに集合な".into(), left: 104, top: 814, width: 440, height: 36, conf: 90.0 },
            OcrLine { text: "部屋で見てみる".into(), left: 108, top: 138, width: 398, height: 78, conf: 90.0 },
            OcrLine { text: "まだ戻らない".into(), left: 96, top: 230, width: 336, height: 51, conf: 90.0 },
        ];
        let blocks = cluster_text_lines(&lines);
        let texts: Vec<&str> = blocks.iter().map(|b| b.combined_text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "部屋で見てみる",
                "まだ戻らない",
                "花村陽介",
                "明日は日曜だし、\n朝イチでジュネスに集合な",
            ]
        );
    }

    #[test]
    fn rejects_chopped_ending_tails_and_merges_wide_dialogue_gaps() {
        assert!(!is_valid_japanese_dialogue("たぜ!", true));
        assert!(!is_valid_japanese_dialogue("てたよ!", true));
        assert!(is_valid_japanese_dialogue("えっ!", true));

        let lines = vec![
            OcrLine { text: "本人だったし、つか".into(), left: 120, top: 760, width: 380, height: 48, conf: 88.0 },
            OcrLine { text: "たぜ!".into(), left: 620, top: 762, width: 110, height: 46, conf: 85.0 },
        ];
        let blocks = cluster_text_lines(&lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].combined_text, "本人だったし、つかたぜ!");
    }

    #[test]
    fn inplace_stacked_choices_never_domino_push_onto_lower_rows() {
        let mk_item = |x: i32, y: i32, actual_w: i32, max_w: i32, h: i32, text: &str| OverlayItem {
            pos_x: x,
            pos_y: y,
            font_size: 20,
            max_width: max_w,
            est_height: h,
            actual_w,
            trans: text.to_string(),
            src_left_mon: x,
            src_w: actual_w,
            src_top_mon: y,
            src_bottom_mon: y + h,
        };

        // 1. Same-row narrowed pill (actual_w=110, stale max_width=260) next to neighbor at x=240.
        let mut same_row = vec![
            mk_item(100, 220, 110, 260, 46, "In the snow"),
            mk_item(240, 220, 160, 160, 46, "Contact me"),
        ];
        resolve_subtitle_collisions(&mut same_row, 1080, &[], true);
        assert_eq!(same_row[0].pos_y, 220);
        assert_eq!(same_row[1].pos_y, 220);

        // 2. 3 stacked choices (y=140, 220, 300) + speaker banner (y=380) with bloated heights (88px).
        let mut stacked = vec![
            mk_item(100, 140, 180, 180, 88, "Calm down."),
            mk_item(100, 220, 220, 220, 88, "Contact Yukiko"),
            mk_item(100, 300, 210, 210, 88, "Contact Chie"),
            mk_item(100, 380, 240, 240, 88, "Yosuke Hanamura"),
        ];
        resolve_subtitle_collisions(&mut stacked, 1080, &[], true);

        // Every pill stays anchored on its own row without cascading downward.
        assert_eq!(stacked[0].pos_y, 140);
        assert_eq!(stacked[1].pos_y, 220);
        assert_eq!(stacked[2].pos_y, 300);
        assert_eq!(stacked[3].pos_y, 380);

        // Upper bloated heights are trimmed so adjacent stacked pills do not overlap.
        assert!(stacked[0].pos_y + stacked[0].est_height <= stacked[1].pos_y);
        assert!(stacked[1].pos_y + stacked[1].est_height <= stacked[2].pos_y);
        assert!(stacked[2].pos_y + stacked[2].est_height <= stacked[3].pos_y);
    }
}
