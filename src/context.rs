use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn context_file(dir: &PathBuf) -> PathBuf {
    dir.join("context.log")
}

pub fn reset(dir: &PathBuf) {
    let _ = fs::write(context_file(dir), "");
}

pub fn append(dir: &PathBuf, role: &str, text: &str) {
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(context_file(dir)) else {
        return;
    };
    let limited: String = text.chars().take(700).collect();
    let cleaned = limited.replace('\n', " ").replace('\r', " ");
    let _ = writeln!(f, "{}: {}", role, cleaned);
}

pub fn last_turns(dir: &PathBuf, max: usize) -> Vec<String> {
    let Ok(raw) = fs::read_to_string(context_file(dir)) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let skip = lines.len().saturating_sub(max);
    lines.drain(..skip);
    lines
}

pub fn with_context(dir: &PathBuf, enabled: bool, text: &str) -> String {
    if enabled {
        let turns = last_turns(dir, 10);
        if !turns.is_empty() {
            let mut out = text.to_string();
            out.push_str("\n--- recent session context ---\n");
            out.push_str(&turns.join("\n"));
            return out;
        }
    }
    text.to_string()
}