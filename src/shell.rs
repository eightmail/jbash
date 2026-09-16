use crate::config::{self, Config};
use crate::context;
use crate::ecosystem::{self, Plugin, Theme};

use nix::pty::{openpty, Winsize};
use nix::sys::termios as term;
use nix::sys::wait::{waitid, waitpid, Id, WaitPidFlag, WaitStatus};
use nix::unistd::{close, dup2, fork, ForkResult, Pid};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::io::{AsRawFd, BorrowedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn resolve_shell(_cfg: &Config) -> String {
    "bash".into()
}

fn rc_file_name() -> &'static str {
    "bashrc"
}

fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

const SEGMENTS_BASH: &str = r#"
# Git branch (fast skip when not in a work tree)
__jbash_seg_git() {
  git rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 0
  __jbash_git_here=1
  local b
  b="$(git symbolic-ref --short HEAD 2>/dev/null || git rev-parse --short HEAD 2>/dev/null)"
  [ -n "$b" ] || return 0
  printf -v "$1" '%s\[\e[36m\]⎇ %s\[\e[0m\] ' "${!1}" "$b"
}

# Uncommitted changes: unstaged ✗, staged ●
__jbash_seg_dirty() {
  [ -n "${__jbash_git_here:-}" ] || return 0
  local d=""
  git diff --quiet 2>/dev/null; [ $? -ne 0 ] && d="${d}✗"
  git diff --cached --quiet 2>/dev/null; [ $? -ne 0 ] && d="${d}●"
  [ -n "$d" ] || return 0
  printf -v "$1" '%s\[\e[33m\][%s]\[\e[0m\] ' "${!1}" "$d"
}

# Execution time of the last command
__jbash_seg_dur() {
  local s="${__jbash__start:-}" d
  [ -n "$s" ] || return 0
  d="$(awk -v s="$s" -v e="$EPOCHREALTIME" 'BEGIN{printf "%.0f", (e-s)*1000}')"
  if [ "${d:-0}" -ge 1000 ]; then
    printf -v "$1" '%s\[\e[33m\]⏱ %.1fs\[\e[0m\] ' "${!1}" "$(awk -v d="${d:-0}" 'BEGIN{printf "%.1f", d/1000}')"
  else
    printf -v "$1" '%s\[\e[33m\]⏱ %sms\[\e[0m\] ' "${!1}" "${d:-0}"
  fi
}

# Active virtualenv / conda environment
__jbash_seg_venv() {
  local vn=""
  [ -n "${VIRTUAL_ENV:-}" ] && vn="$(basename "$VIRTUAL_ENV")"
  [ -z "$vn" ] && [ -n "${CONDA_DEFAULT_ENV:-}" ] && vn="$CONDA_DEFAULT_ENV"
  [ -z "$vn" ] && return 0
  printf -v "$1" '%s\[\e[32m\]🐍 %s\[\e[0m\] ' "${!1}" "$vn"
}

# Non-zero exit code of the last command
__jbash_seg_err() {
  [ "${2:-0}" = 0 ] && return 0
  printf -v "$1" '%s\[\e[31m\]✘ %s\[\e[0m\] ' "${!1}" "$2"
}

# JSON-contributed plugin segment
# usage: __jbash_seg_plugin <timeout> <label> <color> <command> <outvar>
__jbash_seg_plugin() {
  [ "$#" -ge 4 ] || return 0
  local tmo="$1" label="$2" color="$3" cmd="$4" out
  out="$(timeout "$tmo" bash -c "$cmd" 2>/dev/null | head -n1 | tr -d '\r\n')"
  [ -n "$out" ] || return 0
  printf -v "$5" '%s\[\e[%sm\]%s%s\[\e[0m\] ' "${!5}" "$color" "$label" "$out"
}
"#;

const PROMPT_TAIL: &str = r#"
__jbash_precmd() {
  printf '%s' "$?" > "$JBASH_DIR/last-status" 2>/dev/null
  printf '\r\033[K' > /dev/tty 2>/dev/null   # clear any leftover status line
  __jbash__start=
  [[ "$JBASH_THEME" =~ ^(default|modern)$ ]] && trap '__jbash__start=${__jbash__start:-$EPOCHREALTIME}' DEBUG
  __jbash_prompt
}
if [ -n "$PROMPT_COMMAND" ]; then PROMPT_COMMAND="__jbash_precmd; $PROMPT_COMMAND"; else PROMPT_COMMAND="__jbash_precmd"; fi
trap '__jbash__start=${__jbash__start:-$EPOCHREALTIME}' DEBUG
"#;

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Render the `__jbash_prompt` function from an active theme + plugin segments.
fn prompt_script(theme: &Theme, plugins: &[Plugin]) -> String {
    let mut segs: Vec<String> = theme
        .segments
        .iter()
        .map(|s| match s.as_str() {
            "git" => "__jbash_seg_git info".into(),
            "dirty" => "__jbash_seg_dirty info".into(),
            "dur" => "__jbash_seg_dur info".into(),
            "venv" => "__jbash_seg_venv info".into(),
            "err" => "__jbash_seg_err info \"$rc\"".into(),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();

    let picked: Vec<&Plugin> = if theme.plugins.is_empty() {
        plugins.iter().collect()
    } else {
        plugins.iter().filter(|p| theme.plugins.contains(&p.name)).collect()
    };
    for p in &picked {
        segs.push(format!(
            "__jbash_seg_plugin {} {} {} {} info",
            p.timeout,
            sq(&p.label),
            sq(&p.color),
            sq(&p.command),
        ));
    }
    let body = segs.join("\n  ");
    let nl = if theme.newline { "\\n" } else { "" };

    format!(
        r#"
__jbash_prompt() {{
  local dir rc info
  dir="${{PWD/#$HOME/~}}"
  rc="$(cat "$JBASH_DIR/last-status" 2>/dev/null || echo 0)"
  info=""
  {body}
  PS1="{nl}${{info}}\[\e[{nc}m\]$JBASH_NAME\[\e[0m\] \[\e[{pc}m\]$dir\[\e[0m\]> "
}}
"#,
        nl = nl,
        body = body,
        nc = theme.name_color,
        pc = theme.path_color,
    )
}

const HELPERS_BASH: &str = r#"
__jbash_run() {
  local fun="$1"; shift
  local out yn
  # progress (spinner + token usage) is drawn by the Rust helper itself
  if ! out="$( "$JBASH_BIN" "$fun" --plain "$@" )"; then
    return $?
  fi
  [ -z "$out" ] && return 0
  if [[ "$out" == '#'* ]]; then printf '\e[33mAI:\e[0m %s\n' "${out#\# }"; return 0; fi
  if [[ "$out" == 'ai:'* ]]; then printf '\e[33mAI:\e[0m %s\n' "${out#ai: }"; return 0; fi
  printf '  \e[32m\u25b8\e[0m %s\n' "$out"
  if printf '%s\n' "$out" | grep -Eqi 'rm[ ][-a-z]*r|rm[ ][-a-z]*f|dd[ ].*of=/dev/|mkfs\.|shutdown|reboot'; then
    printf '\e[33m  !! looks destructive — double-check.\e[0m\n'
  fi
  if [ "$JBASH_CONFIRM" = 1 ]; then printf '  run? [y/N] '; IFS= read -r yn; else yn=y; fi
  case "$yn" in y|Y|yes) eval -- "$out"; return $? ;; esac
  return 0
}
ai() {
  case "${1:-}" in
    on)  printf 'on' > "$JBASH_DIR/state"; return 0 ;;
    off) printf 'off' > "$JBASH_DIR/state"; return 0 ;;
    "")  printf 'usage: ai <task in English>    ai on | ai off\n'; return 0 ;;
  esac
  __jbash_run ai "$@"
}
ask() {
  if [ -n "$1" ]; then
    # progress (spinner + token usage) is drawn by the Rust helper itself
    "$JBASH_BIN" ask --plain "$@"
  fi
}
fix() {
  local s cmd
  s="$(cat "$JBASH_DIR/last-status" 2>/dev/null || echo 250)"
  [ "$s" = 0 ] && { printf '\e[2mNothing to fix — last command succeeded.\e[0m\n'; return 0; }
  cmd="$(fc -ln -1 2>/dev/null | tail -n1)"
  __jbash_run fix "$cmd" "$s"
}
command_not_found_handle() {
  local w="$1" op
  [ -n "${__jbash_cnf:-}" ] && return 127
  [ "$(cat "$JBASH_DIR/state" 2>/dev/null || echo on)" = off ] && return 127
  printf '( %s is not a command — [1] ask   [2] run anyway   [3] SKIP ) ' "$w"
  IFS= read -r op
  case "$op" in
    1) ask "$*" ;;
    2) printf 'bash: %s: command not found\n' "$w"; return 127 ;;
    *) printf '\e[2mskipped\e[0m\n' ;;
  esac
  return 0
}
"#;


/// Write the injected rc (prompt, ai/ask/fix, error capture) into JBASH_DIR.
pub fn write_rc(cfg: &Config, dir: &Path) -> io::Result<PathBuf> {
    let bin = env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("jbash"))
        .to_string_lossy()
        .to_string();
    let jbash_dir = single_quote(&dir.to_string_lossy());
    let binq = single_quote(&bin);
    let nameq = single_quote(&cfg.prompt_name);
    let modelq = single_quote(&cfg.model);
    let themeq = single_quote(&cfg.theme);
    let confirm = if cfg.confirm { "1" } else { "0" };

    let body = format!(
        "# jbash runtime — generated by jbash.\n\
         JBASH_DIR={jbash_dir}\n\
         JBASH_BIN={binq}\n\
         JBASH_NAME={nameq}\n\
         JBASH_MODEL={modelq}\n\
         JBASH_THEME={themeq}\n\
         JBASH_CONFIRM={confirm}\n\n\
         {user_rc}\n\
         case $- in *i*) ;; *) return ;; esac\n\n\
         mkdir -p \"$JBASH_DIR\" 2>/dev/null\n\
         : > \"$JBASH_DIR/last-err.log\" 2>/dev/null\n\
         exec 2> >(tee -a \"$JBASH_DIR/last-err.log\" >&2)\n\n\
         {prompt}\n\
         {helpers}\n",
        user_rc = "if [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi",
        prompt = {
            let (theme, plugins) = ecosystem::load(cfg);
            format!("{}{}{}", prompt_script(&theme, &plugins), SEGMENTS_BASH, PROMPT_TAIL)
        },
        helpers = HELPERS_BASH,
    );

    let path = dir.join(rc_file_name());
    fs::write(&path, body)?;
    Ok(path)
}

fn set_raw(fd: BorrowedFd<'_>) -> io::Result<term::Termios> {
    let orig = term::tcgetattr(&fd)?;
    let mut raw = orig.clone();
    term::cfmakeraw(&mut raw);
    raw.output_flags |= term::OutputFlags::OPOST | term::OutputFlags::ONLCR;
    term::tcsetattr(&fd, term::SetArg::TCSANOW, &raw)?;
    Ok(orig)
}

unsafe fn term_winsize(fd: i32) -> libc::winsize {
    let mut ws: libc::winsize = std::mem::zeroed();
    libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws);
    ws
}

unsafe fn sync_winsize(master: i32, src: i32) {
    let ws = term_winsize(src);
    let _ = libc::ioctl(master, libc::TIOCSWINSZ, &ws);
}

fn raw_read(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EIO) {
            return Ok(0);
        }
        return Err(e);
    }
    Ok(n as usize)
}

fn raw_write(fd: i32, buf: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < buf.len() {
        let n = unsafe {
            libc::write(
                fd,
                buf[written..].as_ptr() as *const libc::c_void,
                buf.len() - written,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        written += n as usize;
    }
    Ok(())
}

fn relay(from: i32, to: i32) -> io::Result<bool> {
    let mut buf = [0u8; 8192];
    let n = raw_read(from, &mut buf)?;
    if n == 0 {
        return Ok(true);
    }
    raw_write(to, &buf[..n])?;
    Ok(false)
}

struct RawGuard {
    fd: i32,
    saved: Option<term::Termios>,
}

impl RawGuard {
    fn enable(fd: i32) -> Option<Self> {
        let bfd = unsafe { BorrowedFd::borrow_raw(fd) };
        let saved = set_raw(bfd).ok()?;
        Some(RawGuard { fd, saved: Some(saved) })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if let Some(t) = self.saved.take() {
            let bfd = unsafe { BorrowedFd::borrow_raw(self.fd) };
            let _ = term::tcsetattr(bfd, term::SetArg::TCSANOW, &t);
        }
    }
}

fn child_done(child: Pid) -> bool {
    match waitid(
        Id::Pid(child),
        WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
    ) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) => true,
        Err(_) => true,
    }
}

fn exit_code(child: Pid) -> i32 {
    match waitpid(child, None) {
        Ok(WaitStatus::Exited(_, code)) => code,
        Ok(WaitStatus::Signaled(_, sig, _)) => 128 + sig as i32,
        _ => 1,
    }
}

pub const SYS_CMD: &str = "You are an expert Unix shell engineer embedded inside \"jbash\", an interactive drop-in replacement for bash.

You have a run_shell tool that executes commands in the user's session and returns their stdout, stderr and exit code. Whenever the request depends on the real state of the filesystem, processes, or command outputs — inspect FIRST with one or more short read-only run_shell calls (e.g. ls, pwd, cat, which, wc), using the results, then answer. Do not guess or fabricate state you can observe.

Rules:
- The run_shell tool is ONLY for your own investigation. Any command it returns that the safety guard blocks must not be retried; suggest a safe equivalent instead.
- After investigating, reply with EXACTLY ONE shell command line that fulfills the user's request, and nothing else.
- No markdown, no backticks, no code fences, no JSON objects, no `run_shell \"...\"` wrapper. The command must be plain text on a single line.
- The command must be safe, reversible where possible, and NEVER more destructive than the user explicitly asked for.
- Use modern GNU flags, correct quoting, and paths relative to the current working directory when sensible.
- If the request genuinely cannot be expressed as one command line, reply with a single `# ` comment explaining why.";

pub const SYS_ASK: &str = "You are an AI assistant embedded inside the \"jbash\" command-line shell. Answer tersely and directly, like a senior sysadmin, genuinely engaging with whatever the user wrote. You have a run_shell tool that can run commands in the user's session; use it to check real state (files, processes, outputs) only when that improves your answer. If the user's message is a command name that does not exist or gibberish, reply naturally — tell them it is not a command on this system and, when you can, guess what they may have meant or ask a brief clarifying question. Never pretend to run it, never simulate output, and never just echo bare placeholder text. If shell commands are relevant, show them inline as single-line examples, plain text without JSON or run_shell wrappers.";

pub const SYS_FIX: &str = "A shell command failed. Reply with the corrected command line ONLY — a single line, no fences, no explanation, no backticks. If one line cannot fix it, give a short sequence separated by `; `. Prepend any required explanation as one `# ` comment line.";

/// Spawn bash over a pty and relay bytes until it exits.
pub fn interactive(cfg: &Config) -> i32 {
    let dir = config::data_dir();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("state"), "on");
    context::reset(&dir);

    let rc_path = match write_rc(cfg, &dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("jbash: cannot write runtime rc: {e}");
            return 1;
        }
    };

    let shell = resolve_shell(cfg);

    let ws = unsafe { term_winsize(0) };
    let opts = openpty(
        Some(&Winsize {
            ws_row: ws.ws_row,
            ws_col: ws.ws_col,
            ws_xpixel: ws.ws_xpixel,
            ws_ypixel: ws.ws_ypixel,
        }),
        None,
    );
    let fds = match opts {
        Ok(f) => f,
        Err(e) => {
            eprintln!("jbash: openpty failed: {e}");
            return 1;
        }
    };
    let master_fd = fds.master;
    let slave_fd = fds.slave;

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            drop(slave_fd);
            let code = parent_loop(master_fd.as_raw_fd(), child);
            drop(master_fd);
            code
        }
        Ok(ForkResult::Child) => {
            child_setup(shell, master_fd.as_raw_fd(), slave_fd.as_raw_fd(), &dir, &rc_path);
            127
        }
        Err(e) => {
            eprintln!("jbash: fork failed: {e}");
            1
        }
    }
}

fn child_setup(
    shell: String,
    master: i32,
    slave: i32,
    dir: &Path,
    rc_path: &Path,
) {
    let _ = unsafe { libc::setsid() };
    let _ = unsafe { libc::ioctl(slave, libc::TIOCSCTTY, 0) };
    let _ = dup2(slave, 0);
    let _ = dup2(slave, 1);
    let _ = dup2(slave, 2);
    if slave > 2 {
        let _ = close(slave);
    }
    if master > 2 {
        let _ = close(master);
    }

    let mut cmd = Command::new(&shell);
    cmd.env("JBASH_DIR", dir);
    cmd.arg("--rcfile").arg(rc_path).arg("-i");
    let err = cmd.exec();
    let _ = writeln!(io::stderr(), "jbash: exec {shell} failed: {err}");
}

fn parent_loop(master: i32, child: Pid) -> i32 {
    let _guard = RawGuard::enable(0);
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };

    let mut done = false;
    loop {
        unsafe { sync_winsize(master, 0) };

        let mut fds = [
            libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 },
        ];
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, 200) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }

        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match relay(master, 1) {
                Ok(true) => done = true,
                Ok(false) => {}
                Err(_) => done = true,
            }
        }

        if !done && fds[0].revents & libc::POLLIN != 0 {
            match relay(0, master) {
                Ok(true) => {}
                Ok(false) => {}
                Err(_) => done = true,
            }
        }

        if !done && child_done(child) {
            done = true;
        }

        if done {
            while let Ok(false) = relay(master, 1) {}
            break;
        }
    }
    exit_code(child)
}