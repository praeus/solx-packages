//! Bundled prompt loader. Uses `include_str!` so the prompts ship inside the
//! binary (no on-disk lookup, no `BUNDLED_PROMPTS_DIR` resolution).
//!
//! Substitution is plain `{{key}}` / `{{ key }}` replacement — matches the
//! old `prompt_store::render_prompt` semantics from sol-manager.

const IMAGE_DESCRIBE: &str = include_str!("prompts/extraction-image-describe.prompt.txt");
const AUDIO_SYNTHESIZE: &str = include_str!("prompts/extraction-audio-synthesize.prompt.txt");
const VIDEO_SYNTHESIZE: &str = include_str!("prompts/extraction-video-synthesize.prompt.txt");

pub fn load(name: &str) -> Result<&'static str, String> {
    match name {
        "extraction-image-describe.prompt.txt" => Ok(IMAGE_DESCRIBE),
        "extraction-audio-synthesize.prompt.txt" => Ok(AUDIO_SYNTHESIZE),
        "extraction-video-synthesize.prompt.txt" => Ok(VIDEO_SYNTHESIZE),
        other => Err(format!("unknown prompt: {other}")),
    }
}

/// Replace every `{{key}}` (and `{{ key }}`) in `template` with the
/// corresponding `vars[key]` value. Unresolved keys are left in place.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // Find the matching `}}`.
            if let Some(end_rel) = find_close(&bytes[i + 2..]) {
                let end = i + 2 + end_rel;
                let inner = &template[i + 2..end];
                let key = inner.trim();
                if let Some((_, v)) = vars.iter().find(|(k, _)| *k == key) {
                    out.push_str(v);
                } else {
                    // Leave the original placeholder in place.
                    out.push_str(&template[i..=end + 1]);
                }
                i = end + 2;
                continue;
            }
        }
        out.push(template.as_bytes()[i] as char);
        i += 1;
    }
    out
}

/// Return the byte offset of the `}}` that closes a `{{`, or `None`.
fn find_close(s: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < s.len() {
        if s[i] == b'}' && s[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_known_keys() {
        let t = "filename: {{ file_name }}\nsize: {{size}} bytes";
        assert_eq!(
            render(t, &[("file_name", "cat.png"), ("size", "1024")]),
            "filename: cat.png\nsize: 1024 bytes"
        );
    }

    #[test]
    fn leaves_unknown_keys() {
        let t = "hello {{name}} and {{other}}";
        let out = render(t, &[("name", "world")]);
        assert_eq!(out, "hello world and {{other}}");
    }

    #[test]
    fn handles_text_without_placeholders() {
        assert_eq!(render("plain text", &[]), "plain text");
    }

    #[test]
    fn load_returns_all_three_prompts() {
        assert!(load("extraction-image-describe.prompt.txt").is_ok());
        assert!(load("extraction-audio-synthesize.prompt.txt").is_ok());
        assert!(load("extraction-video-synthesize.prompt.txt").is_ok());
        assert!(load("nonexistent").is_err());
    }
}
