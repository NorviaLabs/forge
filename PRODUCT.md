# Product

<!-- impeccable:product-schema 1 -->

## Platform

terminal

Full-screen keyboard-driven TUI (Rust, ratatui + crossterm) on macOS, Linux, and Windows via WSL2. Not web, iOS, or Android; there is no native Windows build.

## Users

Primary user (working hypothesis, not yet validated by anyone outside the founder): terminal-native developers and sysadmins who today run an AI coding agent, an editor (vim), and a shell in separate tmux panes, and pay a context-switch tax jumping between them.

Job (hypothesis): when I'm mid-task juggling an agent, my editor, and a shell, I want the agent, the diff, the file, and the shell in one focused view, so I don't lose my train of thought.

Out of scope (assumption): IDE-first developers (VS Code, Cursor, Copilot). The terminal is the point, not a tolerated constraint.

Adoption is single-player: the person who installs and runs Forge is the only persona. There is no separate buyer.

## Product Purpose

Forge is an open-source terminal coding environment for engineering with AI agents. It lets you run, supervise, and resume AI coding work without leaving the terminal. The agent is part of the environment rather than the whole environment. You can edit and run commands yourself, delegate to the agent, review diffs and approvals, switch between independent sessions, and resume durable state after an interruption.

Success: a developer stays in one workspace for the whole loop (inspect, edit, delegate, run, review, verify, switch, return) instead of stitching tools together.

## Positioning

Most terminal coding agents optimize the prompt-to-patch loop around an agent you drive from a prompt, with an external editor alongside it. Forge puts the agent, a Vim-style editor, file explorer, interactive shell, diffs, approvals, and durable multi-session state in one keyboard-driven workspace. Managed sessions each get an isolated Git worktree, so one session keeps working while you inspect or direct another. The founder's shorthand is "vim + iTerm2 + Claude Code". This framing comes from the README and has not been validated as messaging.

## Operating Context

- Launched with `forge` from the repository being worked on (or `FORGE_WORKSPACE`). First launch walks through theme, directory trust, and provider connect.
- Used in the user's own terminal emulator, alongside or instead of tmux. Minimum frame is 80×18; wider frames add persistent columns.
- Keyboard-first, with Vim-style editing. Also works with a mouse.
- Agent shell commands run inside an OS sandbox (Seatbelt on macOS; bubblewrap + socat on Linux/WSL2). Network egress is denied by default and opened through explicit host rules.
- Sessions, transcripts, task state, and queued follow-ups persist in a local SQLite journal and survive restarts.
- GitHub workflow through `gh`: browsing issues, issue sessions, and PR/check views. Nothing is pushed, PR'd, or merged without an explicit keypress.

## Capabilities and Constraints

- Multiple independent sessions; managed sessions use linked Git worktrees and session-local state. Child agents get durable state and isolated worktrees.
- Model chat with tool use (file read/edit, code search, Git, shell). Provider sign-in, per-session model selection, reasoning effort, MCP servers.
- File explorer, Vim-style editor, embedded interactive shell, diff review, approvals, diagnostics, activity and task state, scratchpad.
- Two built-in themes (`forge-dark` default, `forge-light`) loaded from TOML. Workspace `.forge/themes/` drop-ins can override them.
- Alpha software (currently `0.1.0-beta.x`). Users are told to review every approval and to start in a disposable or backed-up repository.
- License: MIT. Repo: github.com/NorviaLabs/forge.
- Undecided: whether there will be a paid or team tier, and who would pay.

## Brand Commitments

- Name: Forge (published by NorviaLabs).
- Voice: plain and factual. Copy says exactly what happens and what the user must do. No hype and no "AI-powered" puffery.

## Evidence on Hand

- Dogfooding: Forge is built using Forge. The open-source repo is the main credibility asset.
- Demo recording: `.github/assets/demo.gif`.
- An internal black-box JTBD evaluation against a seeded fixture (`docs/jtbd-evaluation.md`, local only). It found strong diagnosis and review, and weak handling of ambiguity and of handoff accuracy.
- Absent, never fabricate: external users (currently 0), testimonials, customer logos, benchmarks against competitors, usage or retention numbers, pricing.

## Product Principles

1. One workspace for the whole engineering loop. The agent is a participant, not the frame.
2. The human stays in control. Approvals, pushes, PRs, and merges are always explicit, and nothing outward-facing happens implicitly.
3. State is durable. Interruptions, restarts, and session switches never lose work or context.
4. Claims must be true. Status, completion, and check results reflect verified state, never the agent's self-report.
5. Keyboard first, terminal native. Respect the user's terminal, theme, and conventions instead of imitating a GUI.

## Accessibility & Inclusion

- WCAG AA in both built-in themes: 4.5:1 for text, 3:1 for structural UI such as pane borders and focus marks. Enforce this in tests.
- NO_COLOR must stay fully usable. No state is carried by color alone; use glyphs, modifiers, or pointers.
- `tui.reduced_motion` must replace animation with static state indicators.
- Every function reachable from the keyboard.
