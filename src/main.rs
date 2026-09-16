// jbash: top-level entry point.
//
// This file is deliberately thin. All it does is turn whatever the user
// typed into argv into a text string, decide whether we are running an
// interactive session or just answering a one-off request, and then hand the
// real work off to the modules below. Keeping the argument parsing here and
// the logic elsewhere makes it easier to reason about the rest of the program.
mod config;
mod context;
mod ecosystem;
mod llm;
mod sandbox;
mod shell;

use nix::unistd::isatty;
use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{exit, Command};

// Most of the shell functions pass the user's sentence as plain argv, which
// bash has already split on whitespace. Stitch it back together here,
// trimming each piece and dropping anything empty so we do not end up with
// double spaces in the middle of the sentence.
fn join_args(args: &[String], skip: usize) -> String {
    args.iter()
        .skip(skip)
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
}

// Log one completed exchange into the session context file, keeping the same
// role labels the model uses ("user" / "assistant") so that a later request
// can be given this history verbatim, like a transcript.
fn append_context(role_a: &str, text_a: &str, role_b: &str, text_b: &str) {
    let dir = config::data_dir();
    context::append(&dir, role_a, text_a);
    context::append(&dir, role_b, text_b);
}

// "ai" turns a sentence into a single runnable command line. The model is
// allowed to inspect the machine first (via the run_shell tool) but the final
// answer has to be one concrete command, which is what gets printed. We also
// remember the exchange so a follow-up "ai" can build on it.
fn cmd_ai(cfg: &config::Config, text: &str) -> i32 {
    let dir = config::data_dir();
    let user = context::with_context(&dir, cfg.context, text);
    match llm::chat_tools(cfg, shell::SYS_CMD, &user, "ai") {
        Ok(raw) => {
            let cmd = llm::extract_command(&raw);
            if cmd.is_empty() {
                eprintln!("jbash: AI returned nothing usable.");
                return 1;
            }
            append_context("user", text, "assistant", &cmd);
            println!("{cmd}");
            0
        }
        Err(e) => {
            eprintln!("jbash: {e}");
            1
        }
    }
}

// "ask" is the freeform channel: the model chats back with a plain answer
// rather than a command. Unlike "ai" it deliberately does NOT pull in the
// session transcript. That keeps every question self-contained so an old
// answer never leaks into (and distorts) the next one.
fn cmd_ask(cfg: &config::Config, text: &str) -> i32 {
    match llm::chat_tools(cfg, shell::SYS_ASK, text, "ask") {
        Ok(raw) => {
            let ans = llm::clean_answer(&raw);
            println!("{ans}");
            0
        }
        Err(e) => {
            eprintln!("jbash: {e}");
            1
        }
    }
}

// "fix" is a repair conversation: give the model the command line that just
// failed, its exit status, and the tail of stderr, and the model comes back
// with the corrected command. The stderr tail is capped deliberately: long
// build logs are mostly noise and would otherwise blow up the prompt.
fn cmd_fix(cfg: &config::Config, command_text: &str, status_text: &str) -> i32 {
    let dir = config::data_dir();
    let err_tail = fs::read_to_string(dir.join("last-err.log"))
        .map(|s| {
            s.chars()
                .rev()
                .take(2400)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        })
        .unwrap_or_default();
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    let user = format!(
        "The command: {command_text}\nExit status: {status_text}\nstderr from the failed run:\n{err_tail}\n(cwd: {cwd})"
    );
    let user = context::with_context(&dir, cfg.context, &user);
    match llm::chat(cfg, shell::SYS_FIX, &user, "fix") {
        Ok(raw) => {
            let cmd = llm::extract_command(&raw);
            if cmd.is_empty() {
                eprintln!("jbash: AI returned nothing usable.");
                return 1;
            }
            append_context("assistant", &raw, "assistant", &cmd);
            println!("{cmd}");
            0
        }
        Err(e) => {
            eprintln!("jbash: {e}");
            1
        }
    }
}

// Help text is a plain multi-line string; there is nothing clever happening
// here. Kept as one printf-style block so the layout stays readable in code.
fn print_help() {
    println!(
        "jbash: an AI copilot shell wrapping your real bash\n\
         \n\
         \x20 jbash                        interactive shell (pty-wrapped bash)\n\
         \x20 jbash <file  or stdin>       piped input runs through plain bash\n\
         \x20 jbash ai 'task'              natural language -> command (prints command)\n\
         \x20 jbash ask 'question'         ask the AI a question\n\
         \x20 jbash fix [cmd] [status]     explain + propose a fix for the failed command\n\
         \x20 jbash -c 'command'           run a command once with the plain shell\n\
         \x20 jbash --install              symlink into ~/.local/bin/jbash\n\
         \x20 jbash --sleeper               force sandboxed AI tool execution (on by default)\n\
         \x20 jbash --activated             disable the sandbox: AI tools get full access\n\
         \n\
         Config file: ~/.jbash_rc  (api_url, model, sandbox, shell, confirm, context, timeout, temp, prompt_name, theme)"
    );
}

// Symlink our own compiled binary into ~/.local/bin so it is on the user's
// PATH without copying. Remove any old link first because the install script
// used to hard-copy, and a stale copy would shadow the release build.
fn install() {
    let exe = env::current_exe().expect("cannot locate own binary");
    let bin_dir = env::var("HOME").unwrap_or_default();
    let dst = PathBuf::from(&bin_dir).join(".local/bin/jbash");
    if let Some(parent_dir) = dst.parent() {
        let _ = fs::create_dir_all(parent_dir);
    }
    let _ = fs::remove_file(&dst);
    match std::os::unix::fs::symlink(&exe, &dst) {
        Ok(()) => println!("installed {} -> {}", dst.display(), exe.display()),
        Err(e) => {
            eprintln!("jbash: install failed: {e}");
            exit(1);
        }
    }
}

// The sandbox override switches.   They are stripped out of argv before the
// subcommand dispatch so they never leak into the text sent to the AI, and
// their effect (on/off) is baked straight into the config, just like a
// JBASH_SANDBOX env override.   If several appear, the last one wins.
// `--sleeper` is sandboxed (the default); `--activated` runs tool commands
// with full access.   The old `--sandbox`/`--insecure` spellings still work
// as deprecated aliases so sessions started by an older rc keep working.
fn apply_sandbox_flags(cfg: &mut config::Config, raw: Vec<String>) -> Vec<String> {
    raw.into_iter()
        .filter(|a| match a.as_str() {
            "--sleeper" | "--sandbox" => {
                cfg.sandbox = true;
                false
            }
            "--activated" | "--insecure" => {
                cfg.sandbox = false;
                false
            }
            _ => true,
        })
        .collect()
}

fn main() {
    let mut cfg = config::load();
    let args = apply_sandbox_flags(&mut cfg, env::args().skip(1).collect());

    // No arguments at all means "run the shell". There are actually two
    // distinct cases hiding under that: a real terminal wants the interactive
    // pty-wrapped session, whereas piped stdin (a script, a heredoc) should
    // just fall through to plain bash so it behaves exactly like `sh`.
    if args.is_empty() {
        let stdin_is_tty = isatty(0).unwrap_or(false);
        let stdout_is_tty = isatty(1).unwrap_or(false);
        if stdin_is_tty && stdout_is_tty {
            if !cfg.sandbox {
                eprintln!("jbash: warning: sandbox disabled (--activated): the AI's tool commands will run with unrestricted access to your home, keys and other secrets. The prompt indicator shows a red dot while this session runs activated.");
            }
            exit(shell::interactive(&cfg));
        }
        // Not a tty (pipes, files, heredocs) -> hand over to the real shell.
        let shell = shell::resolve_shell(&cfg);
        let e = Command::new(&shell).exec();
        eprintln!("jbash: exec {shell} failed: {e}");
        exit(1);
    }

    // Everything below here is the "one-off headless" mode the injected
    // bash functions call into; the interactive shell itself never reaches
    // this point.
    match args[0].as_str() {
        "-h" | "--help" | "help" => print_help(),
        "--install" => install(),
        "-c" => {
            let cmd = args.get(1).cloned().unwrap_or_default();
            let shell = shell::resolve_shell(&cfg);
            let st = Command::new(&shell)
                .arg("-c")
                .arg(&cmd)
                .status()
                .expect("spawn shell");
            exit(st.code().unwrap_or(1));
        }
        "-p" => {
            // Headless "ai": turn the sentence into a command and print it.
            let text = join_args(&args, 1);
            if text.is_empty() {
                eprintln!("jbash: -p needs text");
                exit(1);
            }
            exit(cmd_ai(&cfg, &text));
        }
        "ai" => {
            // The --plain flag (inserted by the bash wrapper) just shifts the
            // argument slice by one; everything else is shared with the plain
            // `jbash ai "text"` invocation.
            let skip = if args.get(1).map(|s| s == "--plain").unwrap_or(false) {
                2
            } else {
                1
            };
            let text = join_args(&args, skip);
            exit(cmd_ai(&cfg, &text));
        }
        "ask" => {
            let skip = if args.get(1).map(|s| s == "--plain").unwrap_or(false) {
                2
            } else {
                1
            };
            let text = join_args(&args, skip);
            exit(cmd_ask(&cfg, &text));
        }
        "fix" => {
            // Usage: fix [--plain] "command" "status"
            let skip = if args.get(1).map(|s| s == "--plain").unwrap_or(false) {
                2
            } else {
                1
            };
            let command_text = args.get(skip).cloned().unwrap_or_default();
            let status_text = args.get(skip + 1).cloned().unwrap_or_default();
            if command_text.is_empty() {
                eprintln!("jbash: fix needs the failed command line (and optionally its status)");
                exit(1);
            }
            exit(cmd_fix(&cfg, &command_text, &status_text));
        }
        other => {
            eprintln!("jbash: unknown argument '{other}' (see jbash --help)");
            exit(2);
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use crate::config::Config;

    fn scrape(cfg: &mut Config, raw: &[&str]) -> Vec<String> {
        apply_sandbox_flags(cfg, raw.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn default_is_sandboxed() {
        assert!(Config::default().sandbox);
    }

    #[test]
    fn activated_switches_sandbox_off() {
        let mut cfg = Config::default();
        let kept = scrape(&mut cfg, &["--activated", "ai", "hello"]);
        assert!(!cfg.sandbox);
        assert_eq!(kept, vec!["ai", "hello"]);
    }

    #[test]
    fn sleeper_switch_keeps_sandbox_on() {
        let mut cfg = Config::default();
        let kept = scrape(&mut cfg, &["--sleeper", "ask", "x"]);
        assert!(cfg.sandbox);
        assert_eq!(kept, vec!["ask", "x"]);
    }

    #[test]
    fn last_flag_wins() {
        let mut cfg = Config::default();
        let kept = scrape(&mut cfg, &["--activated", "--sleeper", "fix"]);
        assert!(cfg.sandbox);
        assert_eq!(kept, vec!["fix"]);

        let mut cfg = Config::default();
        let kept = scrape(&mut cfg, &["--sleeper", "--activated", "-c"]);
        assert!(!cfg.sandbox);
        assert_eq!(kept, vec!["-c"]);
    }

    #[test]
    fn deprecated_aliases_still_work() {
        let mut cfg = Config::default();
        let _ = scrape(&mut cfg, &["--insecure", "ask"]);
        assert!(!cfg.sandbox);

        let mut cfg = Config::default();
        let _ = scrape(&mut cfg, &["--sandbox", "ask"]);
        assert!(cfg.sandbox);
    }

    #[test]
    fn flags_once_inside_the_text_are_left_alone() {
        let mut cfg = Config::default();
        let kept = scrape(&mut cfg, &["ask", "note: the word --sleeper is just prose"]);
        assert!(cfg.sandbox);
        // The whole sentence is a single argv token, so nothing was stripped.
        assert_eq!(kept.len(), 2);
        assert!(kept[1].contains("--sleeper"));
    }
}
