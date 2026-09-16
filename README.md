# jbash — Jason Bourne Again Shell

**jbash** is an AI copilot shell. It is a lightweight Rust wrapper around your
real `bash` that keeps **every native shell behavior** — readline history with
the up-arrow, tab completion, aliases, job control, `cd` persistence — and
layers an AI assistant on top of it.

Instead of reimplementing a shell, jbash launches your actual `bash` over a
pseudo-terminal and injects a tiny startup hook that provides three magic
commands: `ai`, `ask`, and `fix`. Because the engine is your real shell, you
never lose your `.bashrc`, environment, completions, or history.

```
jbash ai  write a one-liner that deletes everything except the newest file
jbash ask why is this machine running 4 redundant systemd-resolved units?
jbash fix      (after a command fails — the AI explains and proposes a fix)
```

While the AI is working, jbash streams the reply and shows an **animated
spinner with the active model name plus a live token count**:

```
⠹ ask (qwen2.5-coder:7b) · ~47 tok…
```

The approximate count updates as tokens arrive; when the stream finishes the
exact total (`· 87 tok`) flashes for a moment and the line is cleared. Press
**Ctrl+C** at any time to abort the request and get your prompt back — because
the progress line lives in the requesting process itself, it is gone the moment
you interrupt, with no stray spinner left behind.

## Features

- **Native shell, guaranteed** — your real bash over a PTY. Up-arrow history,
  completion, `PROMPT_COMMAND`, and everything else just works, because nothing
  is reimplemented.
- **`ai <task>`** — natural language to a shell command. jbash proposes a
  command, warns if it looks destructive, and asks before running it.
- **`ask <question>`** — chat with the assistant about anything.
- **`fix`** — the previous command failed? Type `fix` and jbash captures the
  command, its exit status, and the stderr tail, then proposes a corrected
  command.
- **Natural-language interception** — type something that is not a command but
  *sounds* like an instruction (e.g. `delete the temp files`), and jbash offers
  `[1] convert  [2] ask  [3] run anyway  [4] skip`.
- **`ai on` / `ai off`** — toggle AI interception mid-session; the prompt shows
  the state (`ai` / `ai:off`).
- **Themed prompts** — your prompt can show the current Git branch, uncommitted
  changes, the execution time of the last command, virtualenv/conda status, and
  the last exit code, with colors and icons (see [Themes](#themes)).
- **Session context** — recent `ai`/`ask` turns are sent along as context, so
  the model remembers what you were doing.
- **Tool use / function calling** — `ai` and `ask` hand the model a `run_shell`
  tool. When the request depends on real state (files, processes, outputs), the
  model can run short, safe commands locally and read their results before
  answering, up to 8 rounds. Tool commands that look destructive are blocked by
  a safety guard, and the model is told to suggest a safe alternative. Backends
  without tool-calling support degrade gracefully to plain chat.
- **Live token usage** — the status line shows the streaming token count
  (`· ~47 tok…` while generating, exact total at the end), and the model is
  never shown as "thinking". This only appears while the model is called, never
  during normal commands. Because the progress is drawn by the Rust process
  itself rather than a background shell helper, **Ctrl+C aborts cleanly** — no
  spinner is left running when you interrupt a request.
- **Your `.bashrc` stays intact** — it is sourced unchanged by the injected
  hook.

## Themes

Set `theme=` in `~/.jbash_rc` (or `JBASH_THEME`). Three themes ship:

| Theme     | Prompt                                                             |
|-----------|--------------------------------------------------------------------|
| `default` | one line: `⎇ main [✗●] ⏱1.2s 🐍venv ✘127 jbash ai ~/repo>`          |
| `modern`  | status line above the prompt (starship-style, two lines)           |
| `minimal` | just `jbash ai ~/dir>`                                             |

On `default` and `modern` the prompt segments are:

- `⎇ branch` — current Git branch (cyan), shown only inside a work tree
- `[✗●]` — uncommitted changes: `✗` unstaged, `●` staged
- `⏱ 1.2s` — execution time of the last command (`ms`/`s`)
- `🐍 venv` — active `$VIRTUAL_ENV` / `$CONDA_DEFAULT_ENV`
- `✘ 127` — non-zero exit code of the last command (red)
- `ai` / `ai:off` — AI interception state
- `~`-shortened working directory (bold)

## Requirements

- Linux/macOS and **bash** (the wrapped engine)
- The Rust toolchain (for building): <https://rustup.rs>
- An OpenAI-compatible LLM endpoint such as [Ollama](https://ollama.com)
  (`http://localhost:11434/v1` by default)
- `curl` and `jq` (used by the LLM client)

## Install

### With the install script

```sh
git clone <your-repo-url> jbash && cd jbash
./install.sh
```

The script:

1. builds the release binary with `cargo`,
2. installs it to `~/.local/bin/jbash` (override with `--prefix <dir>`),
3. writes a default `~/.jbash_rc` if you do not have one yet,
4. warns if `~/.local/bin` is missing from your `PATH`.

### Manually

```sh
cargo build --release
./target/release/jbash --install    # symlink into ~/.local/bin/jbash
```

### First run

Start a session, then set the endpoint/model in `~/.jbash_rc`:

```sh
jbash
ai off   # to avoid interference while you set things up
```

## Usage

```
jbash                        interactive shell (pty-wrapped bash)
jbash <file  or stdin>       piped input runs through plain bash
jbash ai 'task'              natural language -> command (prints command)
jbash ask 'question'         ask the AI a question
jbash fix [cmd] [status]     explain + propose a fix for the failed command
jbash -c 'command'           run a command once with the plain shell
jbash --install              symlink into ~/.local/bin/jbash
```

Interactive-only commands (inside an `jbash` session):

```
ai <task>     ask the AI for a command, then confirm before it runs
ask <text>    ask the AI a direct question
fix           propose a fix for the last failed command
ai on|off     toggle natural-language interception; prompt shows ai:off
```

## Configuration — `~/.jbash_rc`

| Key             | Default                  | Meaning                                    |
|-----------------|--------------------------|--------------------------------------------|
| `api_url`       | `http://localhost:11434/v1` | OpenAI-compatible endpoint            |
| `model`         | `qwen2.5-coder:7b`       | Model served by the endpoint               |
| `confirm`       | `1`                      | Ask before running AI-suggested commands   |
| `context`       | `1`                      | Send recent conversation turns as context  |
| `timeout`       | `120`                    | Request timeout in seconds                  |
| `temp`          | `0.1`                    | Sampling temperature for the AI            |
| `prompt_name`   | `jbash`                  | Name shown in the prompt                    |
| `theme`         | `default`                | `default` \| `modern` \| `minimal`          |

Environment overrides (take precedence): `JBASH_API_URL`, `JBASH_MODEL`,
`JBASH_THEME`, and `JBASH_DIR` (runtime directory, default `~/.jbash`).

## Runtime directory — `~/.jbash`

jbash keeps its session state there:

```
~/.jbash/bashrc        generated rc injected into the shell
~/.jbash/state         'on' | 'off' — AI interception toggle
~/.jbash/last-status   exit status of the last command (used by fix)
~/.jbash/last-err.log  stderr tail of the last failed command (used by fix)
~/.jbash/context.log   recent ai/ask turns, used as AI context
```

## How it works

- **PTY wrapper** (`src/shell.rs`): jbash opens a pty, forks, and runs your
  real `bash` with its standard streams on the pty slave. A small poll loop
  relays bytes both ways, keeps the window size in sync, and reaps the child
  when the shell exits. Your terminal therefore talks to **bash itself**, not
  to a reimplementation — which is why history, completion and job control are
  all native.
- **Injected rc**: `PROMPT_COMMAND` saves `$?` to `last-status`, rebuilds the
  themed prompt, and re-arms a `DEBUG`-trap timer that measures the last
  command's execution time; stderr is teed to `last-err.log`; `ai`/`ask`/`fix`
  shell functions simply call the `jbash` binary in `--plain` mode with the raw
  text; a `command_not_found_handle` intercepts non-command input that looks
  like an instruction.
- **Prompt themes**: `__jbash_seg_*` helpers render the Git branch (only inside
  a work tree), dirty state (`✗`/`●`), last-command duration, virtualenv, and
  exit code; `JBASH_THEME` selects `default`, `modern` (two lines), or
  `minimal`. Command duration is timed via a cheap `DEBUG` trap that sets a
  start timestamp (using bash's built-in `$EPOCHREALTIME`, not a fork) cleared
  on the next prompt, so only the actual command runtime is shown.
- **Live status / token usage**: while a request is in flight, the Rust helper
  draws an animated progress line straight to `/dev/tty` — a braille spinner,
  the model name, and a streaming token count (`· ~N tok…`, exact total after
  the final usage chunk). It lives only for the lifetime of the request, so an
  interrupt (Ctrl+C) ends it together with the `curl` process. Writing to
  `/dev/tty` (not stderr) keeps the captured output and the `last-err.log`
  clean; normal commands never touch the line.
- **LLM client** (`src/llm.rs`): a plain `curl` POST to your endpoint. `chat()`
  sends a system + user prompt and extracts a single command line (or answer).
  `chat_tools()` streamlines the request with `stream:true` and
  `stream_options.include_usage:true`, parsing the SSE `data:` lines into an
  accumulated body. It sends OpenAI-style `tools`/`tool_choice:auto` with a
  `run_shell` function; responses are parsed for native `tool_calls` and for
  content-embedded tool-call JSON (handled by Ollama-style backends), each call
  is executed locally in a `timeout`-guarded `bash -c` with a destructive
  command scan, and the stdout/stderr/exit code is fed back as a `role:"tool"`
  message for up to 8 rounds. A final answer that arrives wrapped in JSON or
  `run_shell "…"` is unwrapped to a bare command. If the endpoint rejects tool
  calling or streaming, it transparently falls back to the plain chat path.
- **Config** (`src/config.rs`): `~/.jbash_rc` is parsed into a `Config`, with
  `JBASH_*` environment overrides.

Circuit: your terminal ⇄ pty relay ⇄ **real bash**; `ai`/`ask`/`fix` call out
to the model via `curl`, and confirmed commands are `eval`'d back into the
shell.

## Troubleshooting

- **No status line on Unix tools / output appears instant** — the status line
  is drawn only when there is a controlling TTY (e.g. piping defeats it). Use a
  real terminal.
- **`jbash: exec bash failed`** — bash is not available at the wrapped path;
  install or fix your bash and ensure it is on `PATH`.
- **"AI request failed (curl exit status: 28)"** — the endpoint is
  unreachable/slow; raise `timeout` and verify `api_url`.
- **Typeahead appears on the next prompt** — keystrokes typed while the AI call
  is in flight are buffered by the real shell and replayed after it finishes;
  this mirrors plain bash behavior.
- **fedora / secureblue MOTD or OSC terminal-integration hooks** — these come
  from your `~/.bashrc` chain, which jbash sources unchanged; they are harmless.

## Uninstall

```sh
rm -f ~/.local/bin/jbash ~/.jbash_rc
rm -rf ~/.jbash
```