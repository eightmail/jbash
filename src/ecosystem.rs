// The JSON ecosystem: themes and prompt plugins.
//
// Users can drop plain JSON files into a config directory to shape the prompt
// without touching Rust or bash.   There are two kinds of file, in two
// folders:
//
//   ~/.config/jbash/themes/<name>.json   ->  prompt layouts
//   ~/.config/jbash/plugins/<name>.json  ->  extra prompt segments
//
// A theme decides which of the built-in segments appear (git branch, dirty
// state, command duration, virtualenv, exit code), whether the prompt sits on
// a fresh line, the base colors, and which plugins join in.   A plugin is just
// a segment whose content comes from running a shell command: e.g. the
// current k8s context, a battery percentage, a weather glyph.   Looking the
// files up at startup keeps the runtime fast: no repeated disk reads while
// you are actually typing commands.
use crate::config::Config;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

// A resolved theme.   `plugins` being empty means "use everything that was
// discovered": the more common intent than opting out one by one.
pub struct Theme {
    pub name: String,
    pub segments: Vec<String>,
    pub plugins: Vec<String>, // empty = all discovered plugins
    pub newline: bool,
    pub name_color: String,
    pub path_color: String,
}

// Extra prompt segment contributed by a plugin: run `command`, show `label`
// + first output line in `color`, give it `timeout` seconds before we stop
// waiting.   Big trade-off here: keep the commands cheap.   A slow plugin
// command delays every single prompt draw, which is exactly what drives
// people away from fancy shells.
pub struct Plugin {
    pub name: String,
    pub command: String,
    pub label: String,
    pub color: String,
    pub timeout: u64,
}

// The segments available out of the box, in the order they render.
fn default_segments() -> Vec<String> {
    ["git", "dirty", "dur", "venv", "err"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

// The three layouts compiled in.   "modern" is the two-line starship-flavoured
// one (segments above, command line below), "minimal" is nothing but the
// prompt name and path.   Anything that is not "modern" or "minimal" falls
// back to the classic one-line default, which keeps bad typos in the config
// from leaving the user with an empty prompt.
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

// Where the JSON files are looked for.   JBASH_CONFIG is the escape hatch for
// people who keep their dotfiles in sync across machines and want the whole
// jbash config nested under their own tree.
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

// List the .json files in a subdirectory, sorted by name so the rendered
// prompt is deterministic rather than dependent on readdir order.   A missing
// directory simply yields an empty list: having no themes installed is a
// perfectly valid state.
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

// Read one theme JSON and turn it into a Theme.   Filename acts as a fallback
// identity for files that do not declare a `name`; defaults mirror the
// built-in "default" layout so a sparse file still produces a sane prompt.
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

// Read one plugin JSON.   A plugin without a `command` is rejected outright —
// there is nothing sensible to render for it, so it is better to skip it
// silently than to show an empty label on every prompt.
fn parse_plugin(p: &Path) -> Option<Plugin> {
    let raw = fs::read_to_string(p).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let command = str_field(&v, "command")?;
    Some(Plugin {
        name: str_field(&v, "name").unwrap_or_else(|| stem(p)),
        command,
        label: str_field(&v, "label").unwrap_or_default(),
        color: str_field(&v, "color").unwrap_or_else(|| "2".into()),
        timeout: v
            .get("timeout")
            .and_then(|x| x.as_u64())
            .unwrap_or(2)
            .min(10),
    })
}

// Resolve the active theme: first look for a JSON file whose name matches
// cfg.theme (JBASH_THEME in the config), otherwise fall back to the compiled
// layouts.   The plugin list is all discovered plugins; filtering by theme is
// done by whoever renders the prompt.
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
