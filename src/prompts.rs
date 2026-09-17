// Mode-aware system prompts.
//
// `--sleeper` always uses the strict contract baked into the binary (the
// `SYS_*` constants in shell.rs): a sandbox is doing security work, so its
// wording must be auditable and identical on every install, and bakes straight
// into the versioned code.
//
// `--activated` hands the model the untrimmed session, and with that comes the
// freedom to tune the model's instructions.   The first time activated mode
// runs a request, jbash writes ~/.config/jbash/prompts.json filled with the
// same wording; from then on that file is the source of truth for activated
// sessions, so editing it is how you fine-tune the prompt.   It lives next to
// the theme/plugin JSON files, honouring the same JBASH_CONFIG override.   Any
// read problem (file missing, invalid JSON, a key deleted) falls back to the
// built-ins, so a botched edit can never silently kill the prompt, and sleeper
// mode never consults the file at all.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::config::Config;

#[derive(Clone, Copy)]
pub enum Kind {
    Cmd,
    Ask,
    Fix,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Cmd => "cmd",
            Kind::Ask => "ask",
            Kind::Fix => "fix",
        }
    }

    fn builtin(self) -> &'static str {
        match self {
            Kind::Cmd => crate::shell::SYS_CMD,
            Kind::Ask => crate::shell::SYS_ASK,
            Kind::Fix => crate::shell::SYS_FIX,
        }
    }
}

fn prompts_file() -> PathBuf {
    crate::ecosystem::conf_dir().join("prompts.json")
}

// Populate a missing prompts file with the built-in wording so the very first
// activated run is byte-for-byte equivalent until the user starts editing.
fn write_defaults(path: &Path) {
    let defaults = json!({
        "cmd": Kind::Cmd.builtin(),
        "ask": Kind::Ask.builtin(),
        "fix": Kind::Fix.builtin(),
    });
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body = serde_json::to_string_pretty(&defaults).unwrap_or_default();
    if fs::write(path, body).is_ok() {
        eprintln!(
            "jbash: wrote {} -- edit it to fine-tune the --activated prompts.",
            path.display()
        );
    }
}

// The prompt for the given channel under the current mode.
pub fn for_mode(cfg: &Config, kind: Kind) -> String {
    // Sandboxed mode keeps the auditable built-in, whatever the file says.
    if cfg.sandbox {
        return kind.builtin().to_string();
    }
    let path = prompts_file();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => {
            write_defaults(&path);
            return kind.builtin().to_string();
        }
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(_) => return kind.builtin().to_string(),
    };
    parsed
        .get(kind.key())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| kind.builtin().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(sandbox: bool) -> Config {
        Config {
            sandbox,
            ..Config::default()
        }
    }

    // Point the config dir at a throwaway location so tests never touch the
    // real ~/.config/jbash and never collide with each other.
    fn fake_dir() -> PathBuf {
        std::env::temp_dir().join(format!("jbash-prompts-test-{}", std::process::id()))
    }

    fn setup() {
        std::env::set_var("JBASH_CONFIG", fake_dir());
        let _ = fs::remove_file(prompts_file());
    }

    #[test]
    fn sleeper_uses_builtins_and_ignores_the_file() {
        setup();
        let c = cfg(true);
        assert_eq!(for_mode(&c, Kind::Cmd), Kind::Cmd.builtin());
        assert_eq!(for_mode(&c, Kind::Ask), Kind::Ask.builtin());
        assert_eq!(for_mode(&c, Kind::Fix), Kind::Fix.builtin());
        assert!(
            !prompts_file().exists(),
            "sleeper must never create prompts.json"
        );
    }

    #[test]
    fn activated_writes_defaults_then_serves_them() {
        setup();
        let c = cfg(false);
        assert_eq!(for_mode(&c, Kind::Cmd), Kind::Cmd.builtin());
        assert!(
            prompts_file().exists(),
            "activated must create prompts.json"
        );
        let raw = fs::read_to_string(prompts_file()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["cmd"].as_str().unwrap(), Kind::Cmd.builtin());
        assert_eq!(parsed["ask"].as_str().unwrap(), Kind::Ask.builtin());
        assert_eq!(parsed["fix"].as_str().unwrap(), Kind::Fix.builtin());
    }

    #[test]
    fn activated_serves_fine_tuned_cmd_and_falls_back_for_others() {
        setup();
        fs::create_dir_all(prompts_file().parent().unwrap()).unwrap();
        fs::write(
            prompts_file(),
            serde_json::to_string_pretty(&json!({
                "cmd": "You follow my custom words now.",
            }))
            .unwrap(),
        )
        .unwrap();
        let c = cfg(false);
        assert_eq!(for_mode(&c, Kind::Cmd), "You follow my custom words now.");
        assert_eq!(for_mode(&c, Kind::Ask), Kind::Ask.builtin());
        assert_eq!(for_mode(&c, Kind::Fix), Kind::Fix.builtin());
    }

    #[test]
    fn broken_json_falls_back_to_builtins() {
        setup();
        fs::write(prompts_file(), "{ not json").unwrap();
        let c = cfg(false);
        assert_eq!(for_mode(&c, Kind::Cmd), Kind::Cmd.builtin());
        assert_eq!(for_mode(&c, Kind::Fix), Kind::Fix.builtin());
    }
}
