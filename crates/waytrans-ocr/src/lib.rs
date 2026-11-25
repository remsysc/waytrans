pub mod tesseract;

use waytrans_core::layout::{
    cluster_text_lines, is_valid_japanese_dialogue, is_valid_text_line,
};
use waytrans_core::preprocess::preprocess_frame;
use waytrans_core::{DialogueBlock, Frame, OcrEngine, Rect, Result};

pub use tesseract::{setup_japanese_models, TesseractOcr};

/// Full frame-reading pipeline shared by live mode and fixture tests:
/// preprocess -> OCR -> noise filter -> cluster into dialogue blocks.
pub fn read_blocks_from_frame(
    ocr: &dyn OcrEngine,
    frame: &Frame,
    source_lang: &str,
    psm: i32,
    mask_rects: &[Rect],
    min_confidence: f32,
    min_line_confidence: f32,
) -> Result<Vec<DialogueBlock>> {
    let pre = preprocess_frame(frame, mask_rects);
    if pre.width == 0 || pre.height == 0 {
        return Ok(Vec::new());
    }

    let mut lines = ocr.read_lines(
        &pre.pixels,
        pre.width,
        pre.height,
        source_lang,
        psm,
        min_confidence,
        min_line_confidence,
        pre.coord_scale,
    )?;

    let sl = source_lang.to_ascii_lowercase();
    let is_ja = matches!(sl.as_str(), "ja" | "jp" | "jpn");

    if is_ja {
        lines.retain(|l| is_valid_japanese_dialogue(&l.text, false));
    } else {
        lines.retain(|l| is_valid_text_line(&l.text, source_lang));
    }

    let mut blocks = cluster_text_lines(&lines);
    if is_ja {
        blocks.retain(|b| is_valid_japanese_dialogue(&b.combined_text, true));
    } else {
        blocks.retain(|b| is_valid_text_line(&b.combined_text, source_lang));
    }
    Ok(blocks)
}
