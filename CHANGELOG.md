# Changelog

All notable changes to jbash are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.0-alpha] - 2026-09-16

First tagged release. jbash is usable but still alpha: expect rough edges.

### Added

- Interactive pty-wrapped bash with an AI copilot (`ai`, `ask`, `fix`).
- Sandboxed AI tool execution (on by default): environment scrubbing,
  sensitive-path veto, and secret redaction in captured output.
- `--sleeper` and `--activated` switches (with `--sandbox` / `--insecure`
  aliases) to choose the sandbox mode at launch.
- Prompt indicator dot showing the active mode (green = sleeper, red = activated).
- `--version` / `-V` to print the running version.
- Tag-driven GitHub release workflow producing Linux x86_64 and aarch64
  binaries with SHA-256 checksums.
- `test/pylint`, a repo-wide lint runner covering Rust, shell, Python, JSON,
  and plain-text hygiene.

[Unreleased]: https://github.com/eightmail/jbash/compare/v0.8.0-alpha...HEAD
[0.8.0-alpha]: https://github.com/eightmail/jbash/releases/tag/v0.8.0-alpha
