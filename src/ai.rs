use crate::config::Config;
use std::process::{Command, Stdio};

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

pub fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                match next {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\x08'),
                    'f' => out.push('\x0C'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Ok(code) = u32::from_str_radix(&hex, 16) {
                            if let Some(unicode_char) = char::from_u32(code) {
                                out.push(unicode_char);
                            }
                        }
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            } else {
                out.push('\\');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Simple, robust extractor for `"content": "..."` or `"message": "..."` from JSON
pub fn extract_json_string_value(json: &str, target_key: &str) -> Option<String> {
    let search_pattern = format!("\"{}\"", target_key);
    let mut pos = 0;

    while let Some(key_idx) = json[pos..].find(&search_pattern) {
        let after_key = pos + key_idx + search_pattern.len();
        // find colon
        let rest = &json[after_key..];
        let mut chars = rest.char_indices();
        let mut found_colon = false;
        let mut str_start = None;

        for (idx, c) in chars.by_ref() {
            if !found_colon {
                if c == ':' {
                    found_colon = true;
                } else if !c.is_whitespace() {
                    break;
                }
            } else {
                if c == '"' {
                    str_start = Some(after_key + idx + 1);
                    break;
                } else if !c.is_whitespace() {
                    break;
                }
            }
        }

        if let Some(start) = str_start {
            // Find closing unescaped quote
            let mut end = start;
            let mut escaped = false;
            let bytes = json.as_bytes();

            while end < bytes.len() {
                let b = bytes[end];
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    let raw_val = &json[start..end];
                    return Some(json_unescape(raw_val));
                }
                end += 1;
            }
        }

        pos = after_key;
    }

    None
}

pub fn generate_commit_message(config: &Config, diff: &str) -> Result<String, String> {
    if diff.trim().is_empty() {
        return Err("No diff content to analyze".to_string());
    }

    let url = format!("{}/chat/completions", config.base_url);
    let prompt_escaped = json_escape(&config.prompt);
    let diff_escaped = json_escape(diff);
    let model_escaped = json_escape(&config.model);

    let body = format!(
        r#"{{"model":"{}","messages":[{{"role":"system","content":"{}"}},{{"role":"user","content":"{}"}}],"temperature":0.2}}"#,
        model_escaped, prompt_escaped, diff_escaped
    );

    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-S", "--max-time", "45", "-X", "POST", &url])
        .arg("-H")
        .arg("Content-Type: application/json");

    if !config.api_key.is_empty() {
        cmd.arg("-H")
            .arg(format!("Authorization: Bearer {}", config.api_key));
    }

    cmd.arg("-d")
        .arg(&body)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute curl: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("curl error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check for API error response
    if stdout.contains("\"error\"") {
        if let Some(err_msg) = extract_json_string_value(&stdout, "message") {
            return Err(format!("API Error: {}", err_msg));
        }
    }

    // Extract choices[0].message.content
    if let Some(content) = extract_json_string_value(&stdout, "content") {
        let cleaned = clean_commit_message(&content);
        if !cleaned.is_empty() {
            return Ok(cleaned);
        }
    }

    Err(format!(
        "Failed to parse AI response. Raw output: {}",
        if stdout.len() > 200 {
            &stdout[..200]
        } else {
            &stdout
        }
    ))
}

fn clean_commit_message(raw: &str) -> String {
    let mut s = raw.trim();

    // Strip wrapping markdown code blocks if the model ignored instructions
    if s.starts_with("```") {
        if let Some(first_newline) = s.find('\n') {
            s = &s[first_newline + 1..];
        }
        if s.ends_with("```") {
            s = &s[..s.len() - 3];
        }
    }

    // Strip leading/trailing quotation marks
    let mut trimmed = s.trim().to_string();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        || (trimmed.starts_with('`') && trimmed.ends_with('`'))
    {
        if trimmed.len() >= 2 {
            trimmed = trimmed[1..trimmed.len() - 1].trim().to_string();
        }
    }

    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_escape_and_unescape() {
        let original = "Hello \"world\"\nLine 2\tTabbed\\Backslash";
        let escaped = json_escape(original);
        assert_eq!(
            escaped,
            "Hello \\\"world\\\"\\nLine 2\\tTabbed\\\\Backslash"
        );
        let unescaped = json_unescape(&escaped);
        assert_eq!(unescaped, original);
    }

    #[test]
    fn test_extract_json_content() {
        let json = r#"{"id":"chatcmpl-123","choices":[{"message":{"role":"assistant","content":"feat: add new feature\n\nwith extra detail"}}]}"#;
        let content = extract_json_string_value(json, "content");
        assert_eq!(
            content,
            Some("feat: add new feature\n\nwith extra detail".to_string())
        );
    }

    #[test]
    fn test_extract_json_error() {
        let json = r#"{"error":{"message":"Invalid API key provided","type":"invalid_request_error"}}"#;
        let err = extract_json_string_value(json, "message");
        assert_eq!(err, Some("Invalid API key provided".to_string()));
    }
}
