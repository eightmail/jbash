// Everything that talks to the model lives in this file: building the JSON
// request payloads, streaming the response off the wire, showing the animated
// status line while we wait, and: the interesting part: letting the model
// call back into the machine through a run_shell tool.
//
// The HTTP client is deliberately plain curl + jq (no HTTP library): both are
// universally present on a dev box, the endpoints we target are a tiny subset
// of the OpenAI schema, and any failure surfaces as a readable curl exit
// status rather than a library error nobody can interpret.
use crate::config::Config;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Escape Rust strings for use inside a JSON string literal. We are building
// payloads by hand rather than with a serialiser, so this is the one function
// that has to be right: a stray double-quote in the user's sentence would
// otherwise corrupt the whole request and confuse the parser with a mystery
// 400 from the server.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// The config accepts both "http://host:port/v1" and "http://host:port" as an
// api_url. Normalise so the endpoint always lands on /v1/chat/completions,
// regardless of which spelling the user picked.
fn api_url(cfg: &Config) -> String {
    let mut base = cfg.api_url.trim_end_matches('/').to_string();
    if base.ends_with("/v1") {
        base.truncate(base.len() - 3);
    }
    format!("{base}/v1/chat/completions")
}

// POST the raw payload (already fully escaped) to the endpoint and return the
// raw JSON body; on failure, rebuild a readable error from the last bits of
// stderr so we don't dump a whole /tmp log on the user.
fn post_raw(cfg: &Config, payload: &str) -> Result<String, String> {
    let curl = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "--max-time",
            &cfg.timeout.to_string(),
            "-H",
            "Content-Type: application/json",
            "-d",
            payload,
            &api_url(cfg),
        ])
        .output()
        .map_err(|e| format!("failed to spawn curl: {e}"))?;

    if !curl.status.success() {
        let err = String::from_utf8_lossy(&curl.stderr);
        let tail: String = err
            .chars()
            .rev()
            .take(160)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return Err(format!(
            "AI request failed (curl {status}): {tail}",
            status = curl.status
        ));
    }
    if curl.stdout.is_empty() {
        return Err("AI request failed: empty response".into());
    }
    Ok(String::from_utf8_lossy(&curl.stdout).into_owned())
}

// Feed `body` into `jq -r "<filter>"` and return its stdout. printf is the
// transport because it never mangles special characters the way a heredoc
// might; the filter itself is responsible for pulling the field we want out
// of the response.
fn pipe_fetch(body: &str, filter: &str) -> Result<String, String> {
    let printf = Command::new("printf")
        .arg("%s")
        .arg(body)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn printf: {e}"))?;
    let stdout = printf
        .wait_with_output()
        .map_err(|e| format!("printf failed: {e}"))?;

    let mut jq = Command::new("jq")
        .arg("-r")
        .arg(filter)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn jq: {e}"))?;
    jq.stdin
        .as_mut()
        .ok_or("no jq stdin")?
        .write_all(&stdout.stdout)
        .map_err(|e| e.to_string())?;
    drop(jq.stdin.take());
    let out = jq.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let snippet: String = body.chars().take(200).collect();
        return Err(format!("AI returned no parseable answer: {snippet}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/* ------------------------------------------------------------------ */
/*  Terminal status line (spinner + live token usage)                  */
/* ------------------------------------------------------------------ */

// Colour for the spinner glyph. The default theme goes neon green because
// the katakana rain suits it; every other theme gets a calmer amber so the
// accent colour does not clash with whatever the user designed.
fn glyph_color(theme: &str) -> String {
    if theme.eq_ignore_ascii_case("default") {
        "92".into()
    } else {
        "33".into()
    }
}

// The animated status line. Two properties make it behave in the way people
// actually want:
//   - it is drawn straight to /dev/tty, never to stdout/stderr, so captured
//     output and the logged stderr stay clean;
//   - it lives in the *requesting* process. Interrupt the command (Ctrl+C)
//     and the whole thing disappears with it, no orphaned spinner loop is
//     left drawing over the next prompt.
struct Status {
    tty: Option<Arc<Mutex<std::fs::File>>>,
    detail: Arc<Mutex<String>>,
    running: Arc<AtomicBool>,
}

impl Status {
    // Kick off the render thread if a controlling terminal is available (no
    // tty, e.g. stdout piped, and the whole status line is skipped silently).
    // The random glyph comes from a tiny xorshift PRNG seeded with the time
    // and pid, not a cryptographic RNG: all we need is "not always the same
    // character".
    fn start(job: &str, model: &str, glyph_color: &str) -> Status {
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .ok()
            .map(|f| Arc::new(Mutex::new(f)));
        let detail = Arc::new(Mutex::new(String::new()));
        let running = Arc::new(AtomicBool::new(true));
        if let Some(tty) = tty.clone() {
            let d = detail.clone();
            let r = running.clone();
            let job = job.to_string();
            let model = model.to_string();
            let glyph = glyph_color.to_string();
            thread::spawn(move || {
                let frames = ['⚡', '⠹', '┼', 'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ', 'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ', 'ム', 'メ', 'モ', 'ヤ', 'ユ', 'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ン', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ッ', 'ャ', 'ュ', 'ョ', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '#'];
                let mut rng = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0x9e37_79b9_7f4a_7c15)
                    ^ (std::process::id() as u64).wrapping_mul(0x2545_f491_4f6c_dd1d);
                while r.load(Ordering::Relaxed) {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let ch = frames[(rng as usize) % frames.len()];
                    // ASCII glyphs (digits, '#') are one column wide while the
                    // katakana are two; pad them so the line never shudders
                    // left and right from frame to frame.
                    let g = if ch.is_ascii() {
                        format!("{ch} ")
                    } else {
                        ch.to_string()
                    };
                    let ext = d.lock().map(|x| x.clone()).unwrap_or_default();
                    let line = format!(
                        "\r\x1b[{}m{}\x1b[0m {} (\x1b[1m{}\x1b[0m)\x1b[2m{}\x1b[0m\x1b[K",
                        glyph, g, job, model, ext
                    );
                    if let Ok(mut f) = tty.lock() {
                        let _ = f.write_all(line.as_bytes());
                        let _ = f.flush();
                    }
                    thread::sleep(Duration::from_millis(90));
                }
            });
        }
        Status {
            tty,
            detail,
            running,
        }
    }

    fn detail(&self, d: &str) {
        if self.tty.is_some() {
            if let Ok(mut g) = self.detail.lock() {
                *g = format!(" {}", d);
            }
        }
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(tty) = &self.tty {
            if let Ok(mut f) = tty.lock() {
                let _ = f.write_all(b"\r\x1b[K");
                let _ = f.flush();
            }
        }
    }
}

// One tool call as it arrives from the wire.   Streaming backends send
// `delta.tool_calls[].function.arguments` in fragments, so `args` accumulates
// across chunks for the same `index`.
#[derive(Default, Clone)]
struct ToolCallDelta {
    id: String,
    name: String,
    args: String,
}

fn tool_call_delta_index(tc: &serde_json::Value) -> usize {
    tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize
}

// After the streaming pass, reassemble everything we collected into a single
// body shaped exactly like a non-streaming response.   That lets the existing
// jq-based parser handle streaming and non-streaming identically, which is
// one less code path to keep in sync.
fn recombined_body(content: &str, calls: &[ToolCallDelta]) -> String {
    let tcs: Vec<String> = calls
        .iter()
        .map(|c| {
            format!(
                r#"{{"id":"{}","type":"function","function":{{"name":"{}","arguments":"{}"}}}}"#,
                json_escape(&c.id),
                json_escape(&c.name),
                json_escape(&c.args)
            )
        })
        .collect();
    format!(
        r#"{{"choices":[{{"message":{{"content":"{}","tool_calls":[{}]}}}}]}}"#,
        json_escape(content),
        tcs.join(",")
    )
}

// What accumulates while we read one streaming response off the wire:
// the visible content, the partial tool calls, and (if the backend opted in)
// the final usage numbers.
struct Streamed {
    content: String,
    has_content: bool,
    tool_calls: Vec<ToolCallDelta>,
    round_tokens: u64,
    usage: Option<(u64, u64, u64)>,
}

// Streaming chat completion via a raw curl -N.   Each `data:` line of the SSE
// stream is parsed on the fly; content accumulates and the live token estimate
// is pushed to the status line as it grows.   The one thing to be careful
// about: an HTTP server doing keep-alive may leave the connection hanging even
// after the model finished, so once we have seen [DONE] the curl child is
// killed rather than waited on, and that forced exit is not treated as an
// error.
fn post_stream(cfg: &Config, payload: &str, status: &Status) -> Result<Streamed, String> {
    let mut curl = Command::new("curl")
        .args([
            "-sS",
            "-N",
            "--fail-with-body",
            "--max-time",
            &cfg.timeout.to_string(),
            "-H",
            "Content-Type: application/json",
            "-d",
            payload,
            &api_url(cfg),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn curl: {e}"))?;

    let stdout = curl.stdout.take().ok_or("no curl stdout")?;
    let mut st = Streamed {
        content: String::new(),
        has_content: false,
        tool_calls: Vec::new(),
        round_tokens: 0,
        usage: None,
    };
    let mut junk: Vec<String> = Vec::new();
    let mut done_seen = false;

    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => return Err(format!("curl stream read error: {e}")),
        };
        let t = line.trim();
        let Some(data) = t.strip_prefix("data:") else {
            if !t.is_empty() {
                junk.push(t.to_string());
            }
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            done_seen = true;
            break;
        }
        if data.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => {
                if !data.is_empty() {
                    junk.push(data.to_string());
                }
                continue;
            }
        };
        if let Some(u) = v.get("usage") {
            let g = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
            st.usage = Some((
                g("prompt_tokens"),
                g("completion_tokens"),
                g("total_tokens"),
            ));
        }
        let choice = &v["choices"][0];
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
            if !c.is_empty() {
                st.has_content = true;
                st.content.push_str(c);
                st.round_tokens += 1;
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let idx = tool_call_delta_index(tc);
                if st.tool_calls.len() <= idx {
                    st.tool_calls.resize(idx + 1, ToolCallDelta::default());
                }
                if let Some(idv) = tc.pointer("/id").and_then(|x| x.as_str()) {
                    st.tool_calls[idx].id = idv.to_string();
                }
                if let Some(n) = tc.pointer("/function/name").and_then(|x| x.as_str()) {
                    st.tool_calls[idx].name = n.to_string();
                }
                if let Some(a) = tc.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    st.tool_calls[idx].args.push_str(a);
                }
            }
        }
        if st.has_content {
            status.detail(&format!("· ~{} tok…", st.round_tokens));
        }
    }

    // Done either when [DONE] arrived or the stream EOF'd on its own.   A
    // keep-alive server may hold the socket open after the content, so we
    // kill curl proactively and only treat a genuinely failed process (not a
    // forced exit) as an error.
    let _ = curl.kill();
    let cooked = curl.wait();
    let status_ok = done_seen
        || match &cooked {
            Ok(st) => st.success(),
            Err(_) => false,
        };
    let err_tail = read_stderr_tail(&mut curl);
    if !status_ok {
        let mut info = err_tail;
        if info.is_empty() {
            info = junk.join(";");
        }
        return Err(format!("AI request failed: {info}"));
    }
    Ok(st)
}

fn read_stderr_tail(curl: &mut std::process::Child) -> String {
    let Some(mut e) = curl.stderr.take() else {
        return String::new();
    };
    let mut all = String::new();
    use std::io::Read;
    if e.read_to_string(&mut all).is_err() {
        return String::new();
    }
    all.chars()
        .rev()
        .take(160)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

// The plain, non-tool chat path (used by `fix`, and as the fallback whenever
// tool calling is unavailable).   `job` only names the status line so the
// spinner can say "fix" instead of something generic.
pub fn chat(cfg: &Config, system: &str, user: &str, job: &str) -> Result<String, String> {
    let payload = format!(
        r#"{{"model":"{}","stream":false,"temperature":{},"messages":[{{"role":"system","content":"{}"}},{{"role":"user","content":"{}"}}]}}"#,
        json_escape(&cfg.model),
        cfg.temp,
        json_escape(system),
        json_escape(user)
    );
    let status = Status::start(job, &cfg.model, &glyph_color(&cfg.theme));
    let body = post_raw(cfg, &payload);
    status.stop();
    let body = body?;
    let text = pipe_fetch(&body, ".choices[0].message.content // empty")?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("AI returned an empty answer".into());
    }
    Ok(text)
}

/* ------------------------------------------------------------------ */
/*  Tool-use / function calling                                        */
/* ------------------------------------------------------------------ */

// The only tool the model gets.   Keeping it to a single, general-purpose
// shell runner is what makes this whole design work: the model already knows
// how to inspect and change a Unix system, it just needs the ability to do so
// here. Specialised tools (read file, list dir, ...) would be redundant.
const TOOL_NAME: &str = "run_shell";
const TOOL_DESC: &str = "Run a shell command in the user's jbash session and return its stdout, stderr and exit code. Use it to inspect files, running processes, disk usage, command outputs, or to make small safe changes. Prefer short, reversible, read-only commands unless the task explicitly requires otherwise. Unless jbash was started with --activated, the command runs inside a sandbox: the user's real HOME is replaced with an empty scratch directory, environment variables are scrubbed of credentials and tokens, and commands that read SSH keys, cloud credentials, git credential stores, dotenv files or other secret material are blocked (with a message explaining why). Never try to bypass the sandbox or retrieve secrets: it cannot and must not be done through this tool.";

// Hard ceiling on request->tool->request cycles.   A normal answer needs one
// round; a tricky one needs two or three.   Eight is generous enough that a
// competent model never hits it, but tight enough that a stuck model cannot
// chew up minutes of compute.
const MAX_TOOL_ROUNDS: usize = 8;

struct ToolCall {
    id: String,
    name: String,
    command: String,
}

fn build_tools_field() -> String {
    format!(
        r#""tools":[{{"type":"function","function":{{"name":"run_shell","description":"{}","parameters":{{"type":"object","properties":{{"command":{{"type":"string","description":"The shell command line to run"}}}},"required":["command"]}}}}}}],"tool_choice":"auto""#,
        json_escape(TOOL_DESC)
    )
}

fn msg_role(role: &str, content: &str) -> String {
    format!(
        r#"{{"role":"{}","content":"{}"}}"#,
        role,
        json_escape(content)
    )
}

fn msg_tool(id: &str, content: &str) -> String {
    format!(
        r#"{{"role":"tool","tool_call_id":"{}","content":"{}"}}"#,
        json_escape(id),
        json_escape(content)
    )
}

// Real tool calls are assembled exactly as the OpenAI schema wants them so
// backends that validate strictly don't reject our hand-built messages.
fn msg_assistant_toolcalls(calls: &[ToolCall]) -> String {
    let parts: Vec<String> = calls
        .iter()
        .map(|c| {
            let args = format!(r#"{{"command":"{}"}}"#, json_escape(&c.command));
            format!(
                r#"{{"id":"{}","type":"function","function":{{"name":"{}","arguments":"{}"}}}}"#,
                json_escape(&c.id),
                json_escape(&c.name),
                json_escape(&args),
            )
        })
        .collect();
    format!(
        r#"{{"role":"assistant","content":null,"tool_calls":[{}]}}"#,
        parts.join(",")
    )
}

// Split a response body into (visible answer text, list of tool calls) using a
// single jq filter.   This is the fiddliest part of the whole file, so worth
// explaining:
//
// The filter emits a small framed protocol - each value is prefixed with a
// marker ("MD" for content, "TC" for tool call), then a unit-separator, then
// the unicode length of the value, then another separator, then the value.
// Lengths are measured in characters because jq's `length` on a string counts
// code points, which matches Rust's char iteration.   RFC-style "```json{...}```"
// envelopes in the content are detected and converted into tool calls so that
// backends that cannot emit a real tool_call array still work.
fn parse_response(body: &str) -> Result<(String, Vec<ToolCall>), String> {
    // Each emitted value is prefixed with a tag and a unicode-length, then the value.
    // Lengths match Rust `char` counts because jq's `length` on a string counts code points.
    let filter = r#"
def stripfence: gsub("^```[a-z]*\n?"; "") | gsub("\n?```$"; "") | gsub("`"; "");
.choices[0].message as $m |
  (($m.content // "") as $c |
    if (try (($c|stripfence) | fromjson | .arguments.command != null) catch false)
    then empty
    else ("MD\u001f" + (([$c] | map(length) | join(","))) + "\u001f" + $c) end),
  ($m.tool_calls[]? |
    "TC\u001f" + ([(.id // "")] | map(length) | join(",")) + "\u001f" + (.id // "")
           + "\u001f" + ([(.function.name // "")] | map(length) | join(",")) + "\u001f" + (.function.name // "")
           + "\u001f" + ([(try ((.function.arguments // "{}") | fromjson | .command) catch "")] | map(length) | join(","))
           + "\u001f" + (try ((.function.arguments // "{}") | fromjson | .command) catch "")),
  (($m.content // "") | (try (stripfence | fromjson | select(.arguments.command != null)) catch empty) |
     "TC\u001f" + ([("call_embedded")] | map(length) | join(",")) + "\u001f" + "call_embedded"
           + "\u001f" + ([(.name // "run_shell")] | map(length) | join(",")) + "\u001f" + (.name // "run_shell")
           + "\u001f" + ([(.arguments.command)] | map(length) | join(",")) + "\u001f" + (.arguments.command))"#;
    let raw = pipe_fetch(body, filter)?;

    let chars: Vec<char> = raw.chars().collect();
    let mut content: Option<String> = None;
    let mut calls: Vec<ToolCall> = Vec::new();

    // Walk the framed stream: for each field read the marker, skip the
    // separator, parse the length, then copy exactly that many characters.
    // Unknown bytes are stepped over so a stray newline from jq formatting
    // cannot derail the whole parse.
    let take_value = |chars: &[char], mut i: usize| -> (String, usize) {
        if i < chars.len() && chars[i] == '\u{1f}' {
            i += 1; // separator between fields
        }
        let mut n: usize = 0;
        while i < chars.len() && chars[i].is_ascii_digit() {
            n = n * 10 + chars[i].to_digit(10).unwrap_or(0) as usize;
            i += 1;
        }
        if i < chars.len() && chars[i] == '\u{1f}' {
            i += 1;
        }
        let mut val = String::new();
        let mut got: usize = 0;
        while i < chars.len() && got < n {
            val.push(chars[i]);
            got += 1;
            i += 1;
        }
        (val, i)
    };

    let mut i = 0usize;
    while i + 3 <= chars.len() {
        if chars[i] == 'M' && chars[i + 1] == 'D' && chars[i + 2] == '\u{1f}' {
            let (v, j) = take_value(&chars, i + 3);
            content = Some(v);
            i = j;
        } else if chars[i] == 'T' && chars[i + 1] == 'C' && chars[i + 2] == '\u{1f}' {
            let (id, j) = take_value(&chars, i + 3);
            let (name, j) = take_value(&chars, j);
            let (command, j) = take_value(&chars, j);
            calls.push(ToolCall { id, name, command });
            i = j;
        } else {
            i += 1;
        }
    }

    let Some(content) = content else {
        if calls.is_empty() {
            return Err("AI response contained no answer".into());
        }
        return Ok(("".into(), calls)); // embedded tool call: MD line suppressed by design
    };
    Ok((content, calls))
}

// Actually execute one tool command locally.   The sandbox is applied here:
// the environment is scrubbed (see sandbox.rs) unless the user disabled it
// in the config, and both the command line and its output go through the
// sensitive-content passes.   Wrapped in `timeout` so a runaway (say, a model
// that decides to tail -f something) is cut off instead of hanging the
// request forever, and the result is flattened into a compact "exit / stdout /
// stderr" blob the model can read.   Length is capped: tool output exists to
// inform the answer, not to be pasted back verbatim.
fn run_shell(cfg: &Config, cmd: &str, timeout_secs: u64) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return "[tool] empty command".into();
    }
    if let Some(why) = dangerous(cmd) {
        return format!(
            "[safety guard] command blocked: {why}. Tell the user to run this manually, or suggest a safe equivalent that does not modify the system destructively."
        );
    }
    if cfg.sandbox {
        if let Some(why) = crate::sandbox::veto(cmd) {
            return format!(
                "[sandbox] command blocked: it reads {why}, which is private. Rephrase to avoid touching that; the user can handle it manually."
            );
        }
    }

    let mut c = Command::new("timeout");
    if cfg.sandbox {
        // Hand the command a vacuum-cleaned environment (allowlist + redirected
        // HOME/TMPDIR) instead of inheriting the user's session.   The clear is
        // the important part: layering over the inherited env would only
        // override names, leaving everything else: SSH agent, tokens: intact.
        c.env_clear();
        let dir = crate::config::data_dir();
        for (k, v) in crate::sandbox::sanitized_env(&dir) {
            c.env(k, v);
        }
    }
    let out = c
        .arg(timeout_secs.to_string())
        .arg("bash")
        .arg("-c")
        .arg(cmd)
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return format!("[tool] failed to run command: {e}"),
    };
    let code = match out.status.code() {
        Some(c) => c.to_string(),
        None => match out.status.signal() {
            Some(s) => (128 + s).to_string(),
            None => "?".into(),
        },
    };
    let mut s = String::from("exit: ");
    s.push_str(&code);
    s.push('\n');
    if !out.stdout.is_empty() {
        s.push_str("stdout:\n");
        // Output passes through the redaction sweep even in unsandboxed mode:
        // it is cheap, and a key that leaked once stays out of the model's
        // hands at zero extra risk.
        s.push_str(&crate::sandbox::redact(&String::from_utf8_lossy(
            &out.stdout,
        )));
    }
    if !out.stderr.is_empty() {
        s.push_str("stderr:\n");
        s.push_str(&crate::sandbox::redact(&String::from_utf8_lossy(
            &out.stderr,
        )));
    }
    let s: String = s.chars().take(6000).collect();
    let s = s.trim_end().to_string();
    if s.is_empty() {
        s
    } else {
        format!("{s}\n")
    }
}

// Passive destructive-command scanner, the same gag the interactive
// command_not_found path uses.   It is deliberately a small, auditable list
// of clearly bad things rather than a "comprehensive" (and ultimately
// bypassable) sandbox: its job is to stop the model blasting the box, not to
// be a security boundary.
fn dangerous(cmd: &str) -> Option<&'static str> {
    let c = cmd.to_ascii_lowercase();
    if c.contains("rm -rf /")
        || c.contains("rm -fr /")
        || c.contains("rm -r /")
        || c.contains("rm -rf -- ")
        || c.starts_with("rm -fr") && c.contains('/')
    {
        Some("recursive delete targeting a path")
    } else if c.contains("mkfs") {
        Some("filesystem creation")
    } else if c.contains("dd") && c.contains("of=/dev/") {
        Some("raw device overwrite")
    } else if c.starts_with("shutdown") || c.starts_with("reboot") || c.starts_with("poweroff") {
        Some("system shutdown/reboot")
    } else if c.contains(":(){") || c.contains("fork bomb") {
        Some("fork bomb attempt")
    } else if c.contains("> /dev/sd") || c.contains("> /dev/mem") {
        Some("raw block device overwrite")
    } else {
        None
    }
}

// A one-off progress line printed to the terminal (used for tool activity
// between rounds).   Written to /dev/tty like the spinner so that stdout stays
// parseable for scripted use.
fn tty_progress(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = writeln!(f, "{msg}");
    }
}

// The tool-using chat loop.   Flow:
//   1. send the conversation (+ tools) streaming
//   2. if the reply contains tool calls, run each one and push the results
//      back as `role:"tool"` messages, then loop
//   3. once the model answers with plain text, that is the answer.
// Streaming keeps the spinner's token count live the whole time.   Two
// graceful fallbacks matter: a backend that rejects tools or streaming drops
// to the plain chat path (round 0 only), and repeated identical tool commands
// abort the loop early: that specific signature means the model is stuck
// going in circles and more rounds will not help.
pub fn chat_tools(cfg: &Config, system: &str, user: &str, job: &str) -> Result<String, String> {
    let mut messages = vec![msg_role("system", system), msg_role("user", user)];
    let tool_timeout = cfg.timeout.min(30);
    let mut cumulative: u64 = 0;
    let mut seen_cmds: HashMap<String, usize> = HashMap::new();

    for round in 0..MAX_TOOL_ROUNDS {
        let payload = format!(
            r#"{{"model":"{}","stream":true,"stream_options":{{"include_usage":true}},"temperature":{},"messages":[{}],{}}}"#,
            json_escape(&cfg.model),
            cfg.temp,
            messages.join(","),
            build_tools_field(),
        );

        let status = Status::start(job, &cfg.model, &glyph_color(&cfg.theme));
        let streamed = match post_stream(cfg, &payload, &status) {
            Ok(s) => s,
            Err(_) if round == 0 => {
                // Backend has no streaming/tool support (or it kicked off with
                // a transient failure): degrade to the plain text path using
                // the exact same system and user prompts.
                status.stop();
                return chat(cfg, system, user, job);
            }
            Err(e) => {
                status.stop();
                return Err(e);
            }
        };
        if let Some((_, _, total)) = streamed.usage {
            cumulative += total;
            status.detail(&format!("· {} tok", cumulative));
            // hold the final count on screen for a beat so it is readable
            thread::sleep(Duration::from_millis(160));
        } else if streamed.has_content {
            status.detail(&format!("· ~{} tok…", streamed.round_tokens));
        }
        status.stop();

        let body = recombined_body(&streamed.content, &streamed.tool_calls);
        let (content, calls) = match parse_response(&body) {
            Ok(c) => c,
            Err(_) if round == 0 => return chat(cfg, system, user, job),
            Err(e) => return Err(e),
        };

        // Plain-text answer: the model is done, hand back the reply.
        if calls.is_empty() {
            let text = unwrap_final(&content);
            if text.is_empty() || text.eq_ignore_ascii_case("null") {
                return Err("AI returned an empty answer".into());
            }
            return Ok(text);
        }

        // The model wants to run things.   Guard against the looping failure
        // mode first: the same command proposed twice in one request means the
        // model has no idea what to do and is repeating itself.
        let cmds: Vec<String> = calls.iter().map(|c| c.command.clone()).collect();
        for cmd in &cmds {
            let n = seen_cmds.entry(cmd.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                return Err(
                    "no valid answer could be produced; the reasoning kept repeating itself".into(),
                );
            }
        }

        messages.push(msg_assistant_toolcalls(&calls));

        for c in &calls {
            let last = if c.name != TOOL_NAME {
                // Never saw a stray tool name: tell the model, don't crash.
                format!(
                    "[tool] unknown tool '{name}', expected {TOOL_NAME}",
                    name = c.name
                )
            } else {
                // Print what is being run so the user can see the model working.
                tty_progress(&format!("\x1b[1;35m⚙\x1b[0m {cmd}", cmd = c.command));
                let got = run_shell(cfg, &c.command, tool_timeout);
                if let Some(t) = truncate_for_feed(&got, 90) {
                    tty_progress(&format!("\x1b[2m  → {t}\x1b[0m"));
                }
                got
            };
            messages.push(msg_tool(&c.id, &last));
        }
    }

    // Only reached when every round produced tool calls and never an answer.
    Err("no valid answer could be produced after several attempts".into())
}

// Trim tool output down to its first non-empty line for the one-line progress
// echo on the terminal; building logs get summarised to something readable
// instead of scrolling the user's screen.
fn truncate_for_feed(s: &str, n: usize) -> Option<String> {
    let first = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut out: String = first.chars().take(n).collect();
    if first.chars().count() > n {
        out.push('…');
    }
    Some(out)
}

// Some backends frame the final answer as a JSON tool-call envelope
// (`json { "command": "..." }`, with or without fences) instead of emitting a
// plain command line.   Detect those and pull the inner string out so the
// caller sees a bare command; if nothing matches, the text is returned
// unchanged.
fn unwrap_final(raw: &str) -> String {
    let text = raw.trim().to_string();

    // JSON envelope forms: {"command": ...}, {"arguments":{"command":...}},
    // json{...}, ```json {...}```
    let mut cand = text.trim_start_matches("json").trim();
    cand = cand.trim_start_matches('`').trim();
    if cand.starts_with('{') && cand.ends_with('}') {
        let filter = r#"try (([.command, .arguments.command] | map(select(type == "string" and . != "")) | .[0])) catch empty"#;
        if let Ok(mut v) = pipe_fetch(cand, filter) {
            v = v.trim().to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }

    // Plain wrapper the model sometimes emits: run_shell "cmd" / run_shell('cmd')
    for (open, close) in [("\"", "\""), ("'", "'")] {
        let prefix = format!("run_shell {open}");
        let stripped = text.trim_start().strip_prefix(&prefix);
        if let Some(rest) = stripped {
            if let Some(end) = rest.find(close) {
                let inner = rest[..end].trim();
                if !inner.is_empty() {
                    return inner.to_string();
                }
            }
        }
    }

    text.trim().to_string()
}

// Turn a model reply into a single, runnable command line.   The models we
// target love wrapping commands in code fences and shell-syntax markers, so
// this is mostly fence-stripping plus dropping blank lines and the stray
// backticks that always survive.   Multi-step replies are kept as multiple
// lines (#!/bin/sh-style) since they are meant to run as-is.
pub fn extract_command(raw: &str) -> String {
    let cleaned = if raw.contains("```") {
        let start = raw.find("```").map(|i| i + 3).unwrap_or(0);
        let rest = &raw[start..];
        let end = rest.find("```").map(|i| start + i).unwrap_or(raw.len());
        raw[start..end].to_string()
    } else {
        raw.lines().next().unwrap_or("").to_string()
    };

    let mut lines: Vec<String> = cleaned
        .lines()
        .skip_while(|l| {
            let t = l.trim();
            t.is_empty()
                || matches!(
                    t.to_ascii_lowercase().as_str(),
                    "bash" | "zsh" | "sh" | "shell"
                )
        })
        .map(|l| l.trim_end().to_string())
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }

    let joined = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<&str>>()
        .join("\n");
    joined.replace('`', "").trim().to_string()
}

// The ask path wants prose back, not a command line, so the only cleanup is
// stripping code fences the model tends to add around multi-line answers.
pub fn clean_answer(raw: &str) -> String {
    raw.replace("```", "").trim().to_string()
}

#[cfg(test)]
mod tool_tests {
    // Real end-to-end checks on the execution path, no model involved: run
    // commands through run_shell exactly as chat_tools would, and assert the
    // sandbox actually holds.   JBASH_DIR is pointed at a throwaway dir so the
    // scratch home lands somewhere harmless during the tests.
    use super::*;
    use crate::config::Config;

    fn test_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("jbash-llm-test-{}", std::process::id()))
    }

    fn prep() {
        std::env::set_var("JBASH_DIR", test_dir());
    }

    #[test]
    fn vetos_private_key_reads() {
        prep();
        let cfg = Config::default();
        let out = run_shell(&cfg, "cat ~/.ssh/id_rsa", 5);
        assert!(out.contains("[sandbox] command blocked"), "got: {out}");
    }

    #[test]
    fn sandbox_points_home_at_scratch_dir() {
        prep();
        let cfg = Config::default();
        let real_home = std::env::var("HOME").unwrap_or_default();
        let out = run_shell(&cfg, "printf '%s' \"$HOME\"", 5);
        assert!(out.contains("sandbox"), "HOME was not scrubbed: {out}");
        assert!(!out.contains(&real_home), "real HOME leaked: {out}");
    }

    #[test]
    fn sandbox_strips_ssh_agent_env() {
        prep();
        // Set a fake live agent socket into the parent env; the sandboxed
        // command must not see it.
        std::env::set_var("SSH_AUTH_SOCK", "/fake/agent.sock");
        let cfg = Config::default();
        let out = run_shell(&cfg, "printf '%s' \"${SSH_AUTH_SOCK:-unset}\"", 5);
        assert!(out.contains("unset"), "SSH_AUTH_SOCK leaked: {out}");
    }

    #[test]
    fn redacts_key_material_in_output() {
        prep();
        let cfg = Config::default();
        let out = run_shell(
            &cfg,
            "printf '%s' $'-----BEGIN OPENSSH PRIVATE KEY-----\\nabc\\n-----END OPENSSH PRIVATE KEY-----'",
            5,
        );
        assert!(
            !out.contains("-----BEGIN"),
            "key block made it to output: {out}"
        );
        assert!(out.contains("[REDACTED private key]"), "got: {out}");
    }
}
