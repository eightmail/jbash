use crate::config::Config;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// A prompt layout: which built-in segments render, whether the prompt starts
/// on a fresh line, base colors, and which plugins participate.
pub struct Theme {
    pub name: String,
    pub segments: Vec<String>,
    pub plugins: Vec<String>, // empty = all discovered plugins
    pub newline: bool,
    pub name_color: String,
    pub path_color: String,
}

/// An extra prompt segment contributed by a plugin: runs `command`, shows
/// `label` + first output line in `color`, with `timeout` seconds to complete.
pub struct Plugin {
    pub name: String,
    pub command: String,
    pub label: String,
    pub color: String,
    pub timeout: u64,
}

fn default_segments() -> Vec<String> {
    ["git", "dirty", "dur", "venv", "err"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn builtin_theme(name: &str) -> Theme {
    let (segments, newline): (Vec<String>, bool) = match name.to_ascii_lowercase().as_str() {
        "modern" => (default_segments(), true),
        "minimal" => (Vec::new(), false),
        _ => (default_segments(), false),
    };
    Theme {
        name: name.to_string(),
        segments,
        plugins: Vec::new(),
        newline,
        name_color: "32".into(),
        path_color: "1".into(),
    }
}

fn conf_dir() -> PathBuf {
    if let Ok(d) = std::env::var("JBASH_CONFIG") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".config/jbash");
    }
    std::env::temp_dir().join("jbash-config")
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

fn json_files(sub: &str) -> Vec<PathBuf> {
    let dir = conf_dir().join(sub);
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string()
}

fn parse_theme(p: &Path) -> Option<Theme> {
    let raw = fs::read_to_string(p).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let segments = v
        .get("segments")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_else(default_segments);
    let plugins = v
        .get("plugins")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let colors = v.get("colors").and_then(|x| x.as_object());
    Some(Theme {
        name: str_field(&v, "name").unwrap_or_else(|| stem(p)),
        segments,
        plugins,
        newline: v.get("newline").and_then(|x| x.as_bool()).unwrap_or(false),
        name_color: colors
            .and_then(|c| c.get("name").and_then(|x| x.as_str()))
            .unwrap_or("32")
            .to_string(),
        path_color: colors
            .and_then(|c| c.get("path").and_then(|x| x.as_str()))
            .unwrap_or("1")
            .to_string(),
    })
}

fn parse_plugin(p: &Path) -> Option<Plugin> {
    let raw = fs::read_to_string(p).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let command = str_field(&v, "command")?;
    Some(Plugin {
        name: str_field(&v, "name").unwrap_or_else(|| stem(p)),
        command,
        label: str_field(&v, "label").unwrap_or_default(),
        color: str_field(&v, "color").unwrap_or_else(|| "2".into()),
        timeout: v.get("timeout").and_then(|x| x.as_u64()).unwrap_or(2).min(10),
    })
}

/// Resolve the active theme (JSON theme matching `cfg.theme`, else built-in)
/// plus all discovered plugin segments.
pub fn load(cfg: &Config) -> (Theme, Vec<Plugin>) {
    let theme = json_files("themes")
        .iter()
        .find_map(|p| parse_theme(p).filter(|t| t.name.eq_ignore_ascii_case(&cfg.theme)))
        .unwrap_or_else(|| builtin_theme(&cfg.theme));
    let plugins = json_files("plugins")
        .iter()
        .filter_map(|p| parse_plugin(p))
        .collect();
    (theme, plugins)
}