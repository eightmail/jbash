use crate::config::Config;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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

fn api_url(cfg: &Config) -> String {
    let mut base = cfg.api_url.trim_end_matches('/').to_string();
    if base.ends_with("/v1") {
        base.truncate(base.len() - 3);
    }
    format!("{base}/v1/chat/completions")
}

/// POST a raw payload to the chat completions endpoint; returns the raw JSON body.
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

/// Run `jq -r <filter>` against `body` (fed via printf); returns jq stdout.
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

/// Glyph color for the status spinner: neon green on the default theme
/// (matrix look), yellow elsewhere.
fn glyph_color(theme: &str) -> String {
    if theme.eq_ignore_ascii_case("default") {
        "92".into()
    } else {
        "33".into()
    }
}

/// Animated status line written to the controlling terminal while a model
/// request is in flight. Lives in the *requesting* process, so interrupting
/// the command kills the spinner too (no leftover background loop).
struct Status {
    tty: Option<Arc<Mutex<std::fs::File>>>,
    detail: Arc<Mutex<String>>,
    running: Arc<AtomicBool>,
}

impl Status {
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
                let frames = ['ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ', 'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ', 'ム', 'メ', 'モ', 'ヤ', 'ユ', 'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ン', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ッ', 'ャ', 'ュ', 'ョ', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '#'];
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
                    let g = if ch.is_ascii() { format!("{ch} ") } else { ch.to_string() };
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
        Status { tty, detail, running }
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

/// Partial tool call accumulated from streaming `delta.tool_calls` chunks.
#[derive(Default, Clone)]
struct ToolCallDelta {
    id: String,
    name: String,
    args: String,
}

fn tool_call_delta_index(tc: &serde_json::Value) -> usize {
    tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize
}

/// Rebuild a `{"choices":[{"message":{...}}]}` body from streamed pieces so the
/// existing non-streaming parser can be reused.
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

struct Streamed {
    content: String,
    has_content: bool,
    tool_calls: Vec<ToolCallDelta>,
    round_tokens: u64,
    usage: Option<(u64, u64, u64)>,
}

/// Streaming chat completion via curl: each `data:` SSE line is parsed,
/// content is accumulated and live token usage is pushed to the status line.
/// On HTTP failure the error body is captured for the round-0 fallback path.
fn post_stream(
    cfg: &Config,
    payload: &str,
    status: &Status,
) -> Result<Streamed, String> {
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
            st.usage = Some((g("prompt_tokens"), g("completion_tokens"), g("total_tokens")));
        }
        let choice = &v["choices"][0];
        let Some(delta) = choice.get("delta") else { continue };
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

    // We are done once [DONE] is seen (or the stream EOFs). A keep-alive HTTP
    // server may leave the connection open, so don't block waiting on curl and
    // don't treat the forced exit as a failure: the stream already completed.
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
    all.chars().rev().take(160).collect::<String>().chars().rev().collect()
}

/// OpenAI-compatible chat completion via curl + jq (both required).
/// `job` names the status line (ai/ask/fix) shown while the request runs.
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

const TOOL_NAME: &str = "run_shell";
const TOOL_DESC: &str = "Run a shell command in the user's jbash session and return its stdout, stderr and exit code. Use it to inspect files, running processes, disk usage, command outputs, or to make small safe changes. Prefer short, reversible, read-only commands unless the task explicitly requires otherwise. The command run is safety-scanned and blocked if it looks destructive.";

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
    format!(r#"{{"role":"{}","content":"{}"}}"#, role, json_escape(content))
}

fn msg_tool(id: &str, content: &str) -> String {
    format!(
        r#"{{"role":"tool","tool_call_id":"{}","content":"{}"}}"#,
        json_escape(id),
        json_escape(content)
    )
}

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
    format!(r#"{{"role":"assistant","content":null,"tool_calls":[{}]}}"#, parts.join(","))
}

/// Confirmation token: run a `jq` filter over a JSON response body and split the
/// marker-prefixed, length-prefixed fields jq emits.
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

fn run_shell(cmd: &str, timeout_secs: u64) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return "[tool] empty command".into();
    }
    if let Some(why) = dangerous(cmd) {
        return format!(
            "[safety guard] command blocked: {why}. Tell the user to run this manually, or suggest a safe equivalent that does not modify the system destructively."
        );
    }
    let out = Command::new("timeout")
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
        s.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    if !out.stderr.is_empty() {
        s.push_str("stderr:\n");
        s.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    let s: String = s.chars().take(6000).collect();
    let s = s.trim_end().to_string();
    if s.is_empty() {
        s
    } else {
        format!("{s}\n")
    }
}

/// Passive destructive-command scanner (mirrors the interactive gag).
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

fn tty_progress(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = writeln!(f, "{msg}");
    }
}

/// Tool-use chat loop: the model may call `run_shell`; each call is executed
/// locally and its output is fed back as a `tool` message until the model
/// answers. Requests are streamed so live token usage is shown on the status
/// line. Falls back to a plain (non-tool) request if the backend rejects
/// tool calling. `job` names the status line (ai/ask/fix).
pub fn chat_tools(cfg: &Config, system: &str, user: &str, job: &str) -> Result<String, String> {
    let mut messages = vec![
        msg_role("system", system),
        msg_role("user", user),
    ];
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
                // Backend without streaming/tool support (or a transient
                // failure): degrade to the plain text path with the same prompt.
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
            // let the final count be visible for a beat before the answer
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

        if calls.is_empty() {
            let text = unwrap_final(&content);
            if text.is_empty() || text.eq_ignore_ascii_case("null") {
                return Err("AI returned an empty answer".into());
            }
            return Ok(text);
        }

        let cmds: Vec<String> = calls.iter().map(|c| c.command.clone()).collect();
        for cmd in &cmds {
            let n = seen_cmds.entry(cmd.clone()).or_insert(0);
            *n += 1;
            if *n > 1 {
                return Err(
                    "no valid answer could be produced; the reasoning kept repeating itself"
                        .into(),
                );
            }
        }

        messages.push(msg_assistant_toolcalls(&calls));

        for c in &calls {
            let last = if c.name != TOOL_NAME {
                format!("[tool] unknown tool '{name}', expected {TOOL_NAME}", name = c.name)
            } else {
                tty_progress(&format!("\x1b[1;35m⚙\x1b[0m {cmd}", cmd = c.command));
                let got = run_shell(&c.command, tool_timeout);
                if let Some(t) = truncate_for_feed(&got, 90) {
                    tty_progress(&format!("\x1b[2m  → {t}\x1b[0m"));
                }
                got
            };
            messages.push(msg_tool(&c.id, &last));
        }
    }

    Err("no valid answer could be produced after several attempts".into())
}

fn truncate_for_feed(s: &str, n: usize) -> Option<String> {
    let first = s.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut out: String = first.chars().take(n).collect();
    if first.chars().count() > n {
        out.push('…');
    }
    Some(out)
}

/// If the final reply is a JSON tool-call envelope (some backends emit the final
/// answer as `json { "command": ... }` instead of plain text), unwrap it to the
/// bare command/answer.
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

/// Reduce a model reply to a single, runnable command line.
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
    while lines.last().map_or(false, |l| l.trim().is_empty()) {
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

/// Unescaped multi-line reply for `ask`.
pub fn clean_answer(raw: &str) -> String {
    raw.replace("```", "").trim().to_string()
}