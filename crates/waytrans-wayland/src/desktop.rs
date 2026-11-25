use std::io::Write;
use std::process::{Command, Stdio};

use crate::capture::which_exists;

pub fn notify_fallback(source_text: &str, translated_text: &str, title: &str) {
    if which_exists("notify-send") {
        let body = if source_text.is_empty() {
            translated_text.to_string()
        } else {
            format!("{source_text}\n➔ {translated_text}")
        };
        let _ = Command::new("notify-send")
            .args([
                "-a",
                "waytrans",
                "-i",
                "accessories-character-map",
                title,
                &body,
            ])
            .status();
    } else {
        println!("\n[{title}]\nSource:     {source_text}\nTranslated: {translated_text}");
    }
}

pub fn copy_to_clipboard(text: &str) -> bool {
    if !which_exists("wl-copy") {
        return false;
    }
    let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}
