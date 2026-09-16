// Configuration handling.
//
// Everything the user can tweak without recompiling lives in ~/.jbash_rc (a
// plain `key=value` file).   This module is responsible for three things:
// finding that file, parsing it into a typed Config struct, and layering the
// JBASH_* environment variables on top so that CI or one-off invocations can
// override settings without editing any file.
use std::env;
use std::fs;
use std::path::PathBuf;

// The settings the rest of the program actually cares about.   Everything is
// kept as simple Rust types: no nested structures: because the matching is
// flat and the config file has no hierarchy.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_url: String,
    pub model: String,
    pub confirm: bool,
    pub context: bool,
    pub timeout: u64,
    pub temp: f64,
    pub prompt_name: String,
    pub theme: String, // default | minimal | modern (or a JSON theme name)
    pub sandbox: bool, // scrub env / hide HOME / block sensitive paths for AI tool commands
}

// Sensible defaults for a fresh install: Ollama on localhost, a small coder
// model, confirmation on, and a ninety-second request timeout.   They are
// meant to be usable the moment jbash starts, before the user has written
// any rc file at all.
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
            sandbox: true,
        }
    }
}

// Where jbash keeps its session state (context log, last status, generated
// rc).   The JBASH_DIR env var wins over the ~/.jbash default so the whole
// runtime can be pointed somewhere unusual (tmpfs, a test home, etc.); a last
// fallback keeps things working even when $HOME is unset.
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

// Locate the rc file.   Two candidates are accepted: the classic
// ~/.jbash_rc kept over the years, and a ~/.config/jbash/jbash_rc for people
// who prefer the XDG layout.   First match wins.   If neither exists we fall
// back to a .jbash_rc in the current directory so a repo-local config runs
// with zero setup.
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

// Accept the half dozen spellings people actually write for a boolean
// ("1", "yes", "true", "on") rather than insisting on a single canonical one.
fn parse_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "yes" | "true" | "on"
    )
}

// Read the rc file line by line and fill in any keys found.   Unknown keys are
// silently ignored (forward compatibility), values that fail to parse keep
// their default.   Env overrides are applied last because they are meant to
// win in every case, not just the ones the file got right.
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
            "sandbox" => cfg.sandbox = parse_bool(&val),
            "shell" => {} // kept for backwards compatibility; jbash always wraps bash
            _ => {}
        }
    }
    // Environment overrides, applied in order of precedence.
    if let Ok(v) = env::var("JBASH_API_URL") {
        if !v.is_empty() { cfg.api_url = v; }
    }
    if let Ok(v) = env::var("JBASH_MODEL") {
        if !v.is_empty() { cfg.model = v; }
    }
    if let Ok(v) = env::var("JBASH_THEME") {
        if !v.is_empty() { cfg.theme = v.to_ascii_lowercase(); }
    }
    if let Ok(v) = env::var("JBASH_SANDBOX") {
        if !v.is_empty() { cfg.sandbox = parse_bool(&v); }
    }
    cfg
}