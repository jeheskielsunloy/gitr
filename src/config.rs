use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub prompt: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            prompt: "Generate a concise, conventional git commit message (e.g. 'feat: ...', 'fix: ...') summarizing these changes. Output ONLY the commit message, no markdown codeblocks, no quotes, no extra text.".to_string(),
        }
    }
}

pub fn get_config_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        Some(PathBuf::from(home).join(".config").join("gitr"))
    } else {
        None
    }
}

pub fn load_config() -> Config {
    let mut config = Config::default();

    if let Some(config_dir) = get_config_dir() {
        let config_file = config_dir.join("config");
        if !config_file.exists() {
            let _ = fs::create_dir_all(&config_dir);
            let template = r#"# gitr configuration
# OpenAI, OpenRouter, Groq, Ollama, DeepSeek, etc.
api_key=
base_url=https://api.openai.com/v1
model=gpt-4o-mini
# Optional custom prompt:
# prompt=Write a concise conventional commit message.
"#;
            let _ = fs::write(&config_file, template);
        } else if let Ok(content) = fs::read_to_string(&config_file) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = trimmed.split_once('=') {
                    let key = k.trim();
                    let val = v.trim();
                    match key {
                        "api_key" => config.api_key = val.to_string(),
                        "base_url" => {
                            if !val.is_empty() {
                                config.base_url = val.to_string();
                            }
                        }
                        "model" => {
                            if !val.is_empty() {
                                config.model = val.to_string();
                            }
                        }
                        "prompt" => {
                            if !val.is_empty() {
                                config.prompt = val.to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Environment variable overrides / fallbacks
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if config.api_key.is_empty() && !key.trim().is_empty() {
            config.api_key = key.trim().to_string();
        }
    }
    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
        if !base_url.trim().is_empty() {
            config.base_url = base_url.trim().to_string();
        }
    }
    if let Ok(model) = std::env::var("GITR_MODEL") {
        if !model.trim().is_empty() {
            config.model = model.trim().to_string();
        }
    }
    if let Ok(prompt) = std::env::var("GITR_PROMPT") {
        if !prompt.trim().is_empty() {
            config.prompt = prompt.trim().to_string();
        }
    }

    // Trim trailing slashes from base_url
    if config.base_url.ends_with('/') {
        config.base_url.pop();
    }

    config
}

pub fn load_recents() -> Vec<String> {
    if let Some(config_dir) = get_config_dir() {
        let recents_file = config_dir.join("recents");
        if let Ok(content) = fs::read_to_string(recents_file) {
            return content
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && std::path::Path::new(l).exists())
                .collect();
        }
    }
    Vec::new()
}

pub fn save_recent(path: &str) {
    if let Some(config_dir) = get_config_dir() {
        let _ = fs::create_dir_all(&config_dir);
        let recents_file = config_dir.join("recents");
        let mut list = load_recents();
        list.retain(|p| p != path);
        list.insert(0, path.to_string());
        if list.len() > 10 {
            list.truncate(10);
        }
        let _ = fs::write(recents_file, list.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_autocreate_and_load() {
        let cfg = load_config();
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.model, "gpt-4o-mini");
    }
}
