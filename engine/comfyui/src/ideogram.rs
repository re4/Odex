//! Ideogram 4 specifics. The model was trained on structured JSON captions, and its built-in safety
//! filter refuses more often on short or plain-text prompts (it answers with a flat gray square), so
//! prompts go to it as JSON and a refusal is reported instead of saved as an image.

use serde_json::{json, Value};

/// An Ideogram 4 workflow: the ComfyUI-Ideogram4 nodes, the IdeogramV4 partner node, or ComfyUI's
/// local pipeline (its text encoder loaded as `ideogram4`).
pub fn is_ideogram4(graph: &Value) -> bool {
    graph.as_object().into_iter().flatten().any(|(_, n)| {
        let class = n["class_type"].as_str().unwrap_or_default();
        class.starts_with("Ideogram4") || class.starts_with("IdeogramV4") || n["inputs"]["type"] == "ideogram4"
    })
}

/// The prompt as an Ideogram 4 caption: a JSON object stays as it is, plain text becomes
/// `{"high_level_description": ...}`.
pub fn caption(prompt: &str) -> String {
    match serde_json::from_str::<Value>(prompt.trim()) {
        Ok(v) if v.is_object() => v.to_string(),
        _ => json!({"high_level_description": prompt.trim()}).to_string(),
    }
}

/// What a prompt is about, for file names and the thread: a caption's `high_level_description`,
/// else the prompt itself.
pub fn summary(prompt: &str) -> String {
    serde_json::from_str::<Value>(prompt.trim())
        .ok()
        .and_then(|v| v["high_level_description"].as_str().map(String::from))
        .unwrap_or_else(|| prompt.to_string())
}

/// The model's refusal: a flat gray image with "Image blocked by safety filter" written across
/// the middle. Nearly every pixel is one mid gray (99% in a real one; ordinary pictures under 25%).
pub fn looks_refused(bytes: &[u8]) -> bool {
    let Ok(img) = image::load_from_memory(bytes) else { return false };
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    if w < 16 || h < 16 {
        return false;
    }
    // a 32x32 grid of samples, against their median color
    let mut samples: Vec<[i32; 3]> = (0..32)
        .flat_map(|y| (0..32).map(move |x| (x * (w - 1) / 31, y * (h - 1) / 31)))
        .map(|(x, y)| rgb.get_pixel(x, y).0.map(i32::from))
        .collect();
    samples.sort_by_key(|p| p[0] + p[1] + p[2]);
    let m = samples[samples.len() / 2];
    let gray = (m[0] - m[1]).abs() <= 8 && (m[1] - m[2]).abs() <= 8 && (24..=232).contains(&m[0]);
    let flat = samples.iter().filter(|p| (0..3).all(|c| (p[c] - m[c]).abs() <= 8)).count();
    gray && flat * 10 >= samples.len() * 9
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 64, |x, y| image::Rgb(f(x, y)));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn spots_ideogram4_workflows() {
        let node = |class: &str| json!({"1": {"class_type": class, "inputs": {}}});
        assert!(is_ideogram4(&node("Ideogram4Generate")));
        assert!(is_ideogram4(&node("IdeogramV4")));
        assert!(is_ideogram4(
            &json!({"14": {"class_type": "CLIPLoader", "inputs": {"clip_name": "q.safetensors", "type": "ideogram4"}}})
        ));
        assert!(!is_ideogram4(&node("KSampler")));
        assert!(!is_ideogram4(&node("IdeogramV3")));
    }

    #[test]
    fn captions_and_summaries() {
        assert_eq!(
            caption("  a farmer's scythe in a wheat field "),
            r#"{"high_level_description":"a farmer's scythe in a wheat field"}"#
        );
        let full = r#"{"high_level_description": "A scythe", "style_description": {"medium": "photo"}}"#;
        assert_eq!(caption(full), serde_json::from_str::<Value>(full).unwrap().to_string(), "a caption stays as it is");
        assert_eq!(caption("[1, 2]"), r#"{"high_level_description":"[1, 2]"}"#, "JSON that isn't an object is text");
        assert_eq!(summary(full), "A scythe");
        assert_eq!(summary("a red fox"), "a red fox");
    }

    #[test]
    fn a_flat_gray_image_is_a_refusal() {
        assert!(looks_refused(&png(|_, _| [128, 128, 128])));
        // the real one: gray with a line of light text across the middle
        assert!(looks_refused(&png(|x, y| if (30..34).contains(&y) && x % 3 != 0 {
            [225, 225, 226]
        } else {
            [133, 133, 135]
        })));
        assert!(looks_refused(&png(|x, _| [126 + (x % 3) as u8, 127, 128])), "compression noise");
        assert!(!looks_refused(&png(|x, y| [(x * 4) as u8, (y * 4) as u8, 90])), "a real picture");
        assert!(!looks_refused(&png(|_, _| [200, 40, 40])), "a flat color that isn't gray");
        assert!(!looks_refused(&png(|_, _| [0, 0, 0])), "black");
        assert!(!looks_refused(b"not an image"));
    }
}
