use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use waytrans_core::{Frame, Rect};
use waytrans_ocr::{read_blocks_from_frame, TesseractOcr};

#[derive(Deserialize)]
struct ExpectedSpec {
    #[serde(default)]
    contains: Vec<String>,
    max_blocks: Option<usize>,
    #[serde(default)]
    mask_rects: Vec<[i32; 4]>,
}

#[test]
fn real_game_frames_match_expected_json() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixtures_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("fixtures");

    let expected_raw =
        fs::read_to_string(fixtures_dir.join("expected.json")).expect("read expected.json");
    let specs: HashMap<String, ExpectedSpec> =
        serde_json::from_str(&expected_raw).expect("parse expected.json");

    let ocr = TesseractOcr::new(true, true);
    if ocr.check_available().is_err() {
        eprintln!("Skipping fixture OCR test: libtesseract not available");
        return;
    }

    for (name, spec) in specs {
        let img_path = fixtures_dir.join(format!("{name}.png"));
        if !img_path.exists() {
            eprintln!("Skipping missing local fixture: {}", img_path.display());
            continue;
        }
        let dyn_img = image::open(&img_path).expect("open fixture png").to_rgb8();
        let frame = Frame::new(dyn_img.width(), dyn_img.height(), dyn_img.into_raw());
        let masks: Vec<Rect> = spec
            .mask_rects
            .iter()
            .map(|&[x, y, w, h]| Rect::from_xywh(x, y, w, h))
            .collect();

        let blocks = read_blocks_from_frame(&ocr, &frame, "ja", 11, &masks, 30.0, 62.0)
            .expect("read_blocks_from_frame");
        let texts: Vec<String> = blocks.into_iter().map(|b| b.combined_text).collect();

        for want in &spec.contains {
            assert!(
                texts.iter().any(|t| t.contains(want)),
                "{name}: expected {want:?} in OCR blocks, got {texts:?}"
            );
        }
        if let Some(max_blocks) = spec.max_blocks {
            assert!(
                texts.len() <= max_blocks,
                "{name}: expected <= {max_blocks} blocks, got {texts:?}"
            );
        }
    }
}
