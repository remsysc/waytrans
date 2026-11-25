use once_cell::sync::Lazy;
use regex::Regex;

use crate::types::{significant_overlap, DialogueBlock, Rect};

static SOFT_PUNCT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[…・.。、\s]+|[…・.。、\s]+$").expect("valid regex"));

/// Character-level LCS ratio in `[0.0, 1.0]`.
pub fn text_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    if ac.is_empty() || bc.is_empty() {
        return 0.0;
    }
    let m = ac.len();
    let n = bc.len();
    let mut prev = vec![0usize; n + 1];
    let mut curr = vec![0usize; n + 1];
    for &ca in &ac {
        for (j, &cb) in bc.iter().enumerate() {
            curr[j + 1] = if ca == cb {
                prev[j] + 1
            } else {
                prev[j + 1].max(curr[j])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.fill(0);
    }
    (2.0 * prev[n] as f64) / ((m + n) as f64)
}

fn strip_soft_punct(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(m) = SOFT_PUNCT_RE.find(trimmed) {
        let _ = m;
    }
    // Strip both ends via regex replace_all
    trimmed
}

fn clean_soft_punct(s: &str) -> String {
    let t = s.trim();
    let stripped = SOFT_PUNCT_RE.replace_all(t, "");
    if stripped.is_empty() {
        t.to_string()
    } else {
        stripped.into_owned()
    }
}

/// True if two OCR strings represent the same underlying source text.
pub fn is_same_source_text(a: &str, b: &str, min_similarity: f64) -> bool {
    let sa = a.trim();
    let sb = b.trim();
    if sa == sb {
        return true;
    }
    if sa.is_empty() || sb.is_empty() {
        return false;
    }
    let na = clean_soft_punct(sa);
    let nb = clean_soft_punct(sb);
    if na == nb {
        return true;
    }
    if na.chars().count().min(nb.chars().count()) <= 4 {
        return false;
    }
    let _ = strip_soft_punct;
    text_similarity(&na, &nb) >= min_similarity
}

/// True when `new_text` is a significantly longer continuation/completion of `old_text`.
pub fn is_grown_sentence(old_text: &str, new_text: &str) -> bool {
    let old_clean = clean_soft_punct(old_text);
    let new_clean = clean_soft_punct(new_text);
    if old_clean.is_empty() {
        return false;
    }
    if new_clean.chars().count() < old_clean.chars().count() + 3 {
        return false;
    }
    new_clean.starts_with(&old_clean) || new_clean.contains(&old_clean)
}

#[inline]
pub fn same_place(a: &DialogueBlock, b: &DialogueBlock, tol: i32) -> bool {
    (a.left - b.left).abs() <= tol && (a.top - b.top).abs() <= tol
}

#[derive(Debug, Clone)]
pub struct StickySubtitle {
    pub source_text: String,
    pub trans: String,
    pub src_rect: Rect,
    pub pos_x: i32,
    pub pos_y: i32,
    pub font_size: i32,
    pub max_width: i32,
    pub est_height: i32,
    pub src_patch_sig: Option<Vec<u8>>,
    pub border_sig: Option<Vec<u8>>,
    pub miss_count: usize,
}

#[derive(Debug)]
pub struct ReconcileResult {
    pub surviving: Vec<StickySubtitle>,
    pub new_blocks: Vec<DialogueBlock>,
    pub removed_any: bool,
    pub needs_followup_scan: bool,
}

pub struct StableReadTracker {
    required: usize,
    min_similarity: f64,
    seen: Vec<(DialogueBlock, usize)>,
}

impl StableReadTracker {
    pub fn new(required: usize, min_similarity: f64) -> Self {
        Self {
            required: required.max(1),
            min_similarity,
            seen: Vec::new(),
        }
    }

    pub fn update<F>(&mut self, blocks: &[DialogueBlock], fast_track_fn: F) -> (Vec<DialogueBlock>, bool)
    where
        F: Fn(&DialogueBlock) -> bool,
    {
        let mut new_seen = Vec::with_capacity(blocks.len());
        let mut stable = Vec::new();
        let mut pending = false;

        for b in blocks {
            let curr_text = b.combined_text.trim();
            let mut count = 1usize;
            for (prev, prev_count) in &self.seen {
                let prev_text = prev.combined_text.trim();
                let is_typewriter_growing = curr_text.chars().count() > prev_text.chars().count()
                    && !prev_text.is_empty()
                    && curr_text.starts_with(prev_text);
                if !is_typewriter_growing
                    && same_place(prev, b, 40)
                    && text_similarity(prev_text, curr_text) >= self.min_similarity
                {
                    count = prev_count + 1;
                    break;
                }
            }
            new_seen.push((b.clone(), count));
            if count >= self.required || fast_track_fn(b) {
                stable.push(b.clone());
            } else {
                pending = true;
            }
        }
        self.seen = new_seen;
        (stable, pending)
    }

    pub fn reset(&mut self) {
        self.seen.clear();
    }
}

pub struct SubtitleReconciler {
    miss_limit: usize,
}

impl SubtitleReconciler {
    pub fn new(miss_limit: usize) -> Self {
        Self {
            miss_limit: miss_limit.max(1),
        }
    }

    pub fn reconcile<FI, FS, FR>(
        &self,
        sticky_subtitles: &[StickySubtitle],
        raw_blocks: &[DialogueBlock],
        stable_blocks: &[DialogueBlock],
        inplace_unchanged_fn: Option<FI>,
        source_unchanged_fn: Option<FS>,
        refresh_patch_fn: Option<FR>,
    ) -> ReconcileResult
    where
        FI: Fn(&StickySubtitle) -> bool,
        FS: Fn(&StickySubtitle) -> bool,
        FR: Fn(&StickySubtitle) -> Option<Vec<u8>>,
    {
        let mut surviving: Vec<StickySubtitle> = Vec::new();
        let mut patch_locked_indices: Vec<usize> = Vec::new();
        let mut removed_any = false;

        for sub_orig in sticky_subtitles {
            let mut sub = sub_orig.clone();

            if let Some(ref f_inplace) = inplace_unchanged_fn {
                if f_inplace(&sub) {
                    sub.miss_count = 0;
                    surviving.push(sub);
                    continue;
                }
            }

            let matched_raw = raw_blocks.iter().any(|rb| {
                significant_overlap(sub.src_rect, rb.rect(), 0.35)
                    && is_same_source_text(&sub.source_text, &rb.combined_text, 0.75)
                    && !is_grown_sentence(&sub.source_text, &rb.combined_text)
            });
            if matched_raw {
                sub.miss_count = 0;
                if let (Some(ref f_src), Some(ref f_refresh)) = (&source_unchanged_fn, &refresh_patch_fn) {
                    if !f_src(&sub) {
                        if let Some(sig) = f_refresh(&sub) {
                            sub.src_patch_sig = Some(sig);
                        }
                    }
                }
                surviving.push(sub);
                continue;
            }

            let grown_in_stable = stable_blocks.iter().any(|sb| {
                significant_overlap(sub.src_rect, sb.rect(), 0.30)
                    && is_grown_sentence(&sub.source_text, &sb.combined_text)
            });
            if grown_in_stable {
                removed_any = true;
                continue;
            }

            if let Some(ref f_src) = source_unchanged_fn {
                if f_src(&sub) {
                    sub.miss_count = 0;
                    let idx = surviving.len();
                    surviving.push(sub);
                    patch_locked_indices.push(idx);
                    continue;
                }
            }

            let replaced_by_new = stable_blocks.iter().any(|sb| {
                significant_overlap(sub.src_rect, sb.rect(), 0.30)
                    && (!is_same_source_text(&sub.source_text, &sb.combined_text, 0.80)
                        || is_grown_sentence(&sub.source_text, &sb.combined_text))
            });
            if replaced_by_new {
                removed_any = true;
                continue;
            }

            sub.miss_count += 1;
            let source_changed = source_unchanged_fn
                .as_ref()
                .map(|f_src| sub.src_patch_sig.is_some() && !f_src(&sub))
                .unwrap_or(false);
            if !source_changed && sub.miss_count < self.miss_limit {
                surviving.push(sub);
            } else {
                removed_any = true;
            }
        }

        let mut new_blocks = Vec::new();
        for sb in stable_blocks {
            let already_displayed = surviving.iter().enumerate().any(|(idx, s)| {
                significant_overlap(s.src_rect, sb.rect(), 0.35)
                    && (patch_locked_indices.contains(&idx)
                        || (is_same_source_text(&s.source_text, &sb.combined_text, 0.80)
                            && !is_grown_sentence(&s.source_text, &sb.combined_text)))
            });
            if !already_displayed {
                new_blocks.push(sb.clone());
            }
        }

        let needs_followup_scan = removed_any || surviving.iter().any(|s| s.miss_count > 0);
        ReconcileResult {
            surviving,
            new_blocks,
            removed_any,
            needs_followup_scan,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(text: &str, left: i32, top: i32, right: i32, bottom: i32) -> DialogueBlock {
        DialogueBlock {
            lines: Vec::new(),
            left,
            top,
            right,
            bottom,
            combined_text: text.to_string(),
            avg_line_height: bottom - top,
        }
    }

    #[test]
    fn grown_completed_sentence_replaces_truncated_subtitle() {
        let rec = SubtitleReconciler::new(2);
        let truncated = StickySubtitle {
            source_text: "しかもなんか、バラエティ".into(),
            trans: "What's more, it's a variety teddy".into(),
            src_rect: Rect { left: 80, top: 760, right: 520, bottom: 825 },
            pos_x: 80,
            pos_y: 760,
            font_size: 22,
            max_width: 440,
            est_height: 65,
            src_patch_sig: Some(vec![1; 256]),
            border_sig: None,
            miss_count: 0,
        };
        let completed = blk("しかもなんか、バラエティ番組みたいな…", 80, 760, 920, 825);
        let res = rec.reconcile(
            &[truncated],
            std::slice::from_ref(&completed),
            std::slice::from_ref(&completed),
            None::<fn(&StickySubtitle) -> bool>,
            Some(|_: &StickySubtitle| true),
            None::<fn(&StickySubtitle) -> Option<Vec<u8>>>,
        );
        assert!(res.surviving.is_empty());
        assert_eq!(res.new_blocks, vec![completed]);
        assert!(res.removed_any);
    }

    #[test]
    fn short_strings_not_conflated_and_soft_punct_matched() {
        assert!(!is_same_source_text("行く", "行こう", 0.85));
        assert!(!is_same_source_text("本当？", "本当！", 0.85));
        assert!(is_same_source_text(
            "マヨナカテレビを見てみようか",
            "マヨナカテレビを見てみようか…",
            0.85
        ));
    }
}
