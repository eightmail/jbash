use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub api_url: String,
    pub model: String,
    pub confirm: bool,
    pub context: bool,
    pub timeout: u64,
    pub temp: f64,
    pub prompt_name: String,
    pub theme: String, // default | minimal | modern
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_url: "http://localhost:11434/v1".into(),
            model: "qwen2.5-coder:7b".into(),
            confirm: true,
            context: true,
            timeout: 90,
            temp: 0.1,
            prompt_name: "jbash".into(),
            theme: "default".into(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = env::var("JBASH_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(h) = env::var("HOME") {
        return PathBuf::from(h).join(".jbash");
    }
    env::temp_dir().join("jbash")
}

pub fn rc_path() -> PathBuf {
    if let Ok(h) = env::var("HOME") {
        let p = PathBuf::from(h);
        for f in [".jbash_rc", ".config/jbash/jbash_rc"] {
            let cand = p.join(f);
            if cand.is_file() {
                return cand;
            }
        }
    }
    env::current_dir().unwrap_or_default().join(".jbash_rc")
}

fn parse_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "yes" | "true" | "on"
    )
}

pub fn load() -> Config {
    let mut cfg = Config::default();
    let path = rc_path();
    let raw = fs::read_to_string(&path).unwrap_or_default();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key = line[..eq].trim().to_ascii_lowercase();
        let val = line[eq + 1..].trim().to_string();
        match key.as_str() {
            "api_url" | "server" | "api" => cfg.api_url = val,
            "model" => cfg.model = val,
            "confirm" => cfg.confirm = parse_bool(&val),
            "context" => cfg.context = parse_bool(&val),
            "timeout" => cfg.timeout = val.parse().unwrap_or(cfg.timeout),
            "temp" | "temperature" => cfg.temp = val.parse().unwrap_or(cfg.temp),
            "prompt_name" | "name" => cfg.prompt_name = val,
            "theme" => cfg.theme = val.to_ascii_lowercase(),
            "shell" => {} // kept for backwards compatibility; jbash always wraps bash
            _ => {}
        }
    }
    if let Ok(v) = env::var("JBASH_API_URL") {
        if !v.is_empty() { cfg.api_url = v; }
    }
    if let Ok(v) = env::var("JBASH_MODEL") {
        if !v.is_empty() { cfg.model = v; }
    }
    if let Ok(v) = env::var("JBASH_THEME") {
        if !v.is_empty() { cfg.theme = v.to_ascii_lowercase(); }
    }
    cfg
}