// Session context, preserved between requests.
//
// Every ai/ask/fix exchange is appended to a flat log file (context.log) in
// the runtime directory.   It is deliberately plain-text "role: text" lines
// rather than anything structured: that keeps it greppable and lets the user
// see exactly what is about to be sent to the model.   When context is
// enabled, the most recent handful of lines are pasted into the next prompt so
// the model can follow along with what was being worked on.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn context_file(dir: &PathBuf) -> PathBuf {
    dir.join("context.log")
}

pub fn reset(dir: &PathBuf) {
    let _ = fs::write(context_file(dir), "");
}

// Append one side of a turn.   Lines are trimmed to a bounded length and any
// newlines flattened so the log keeps one exchange per line: a raw sentence
// could otherwise span dozens of lines and push later context past the cap.
pub fn append(dir: &PathBuf, role: &str, text: &str) {
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(context_file(dir)) else {
        return;
    };
    let limited: String = text.chars().take(700).collect();
    let cleaned = limited.replace('\n', " ").replace('\r', " ");
    let _ = writeln!(f, "{}: {}", role, cleaned);
}

// Pull back the most recent `max` lines of the log, oldest first.   Nothing
// clever here: the file may be huge after a long session, so callers should
// keep `max` small.
pub fn last_turns(dir: &PathBuf, max: usize) -> Vec<String> {
    let Ok(raw) = fs::read_to_string(context_file(dir)) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let skip = lines.len().saturating_sub(max);
    lines.drain(..skip);
    lines
}

// Build the user prompt for a request, optionally glueing the recent history
// onto the end.   The separator line is unashamedly visible: the model should
// know those lines are transcript, not part of its instructions.
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