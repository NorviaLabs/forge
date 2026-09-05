# Investigation record

Final baseline: `dfa46a07aa43551395ba7cb6b4867ab2d8a9c324`, fetched from `origin/main` on 2026-09-05. Isolated branch `feat/astra-tui-2026`. Final release build completed in 45.36s after the initial full build; `forge --version` returned `forge 0.1.0-beta.9`. Main advanced during the session from `25f0cb82a86482a3a588e294b7f992355760ef07`; that release-only diff changed `Cargo.toml` and `Cargo.lock`, with no TUI/runtime source changes. The complete scenario run used the source-identical beta.8 binary, and the beta.9 binary received a fresh 120×35 real-PTY startup verification.

Real PTYs: dedicated tmux sessions `astra-forge-2026` and `astra-fixture-2026`. Existing user tmux sessions were left alone. Host reports iTerm.app, xterm-256color, truecolor. Captures are terminal cell text, not TestBackend output. Font shape is a separate validation concern.

## Live observations (UTC)

- 09:05:55 — Started freshly built Forge at 120×35 against the isolated baseline checkout. Files search occupies a nested three-row border plus separator. With no file open, conversation receives the remaining width. Focused composer has a thick full border. Startup shows a task database UNIQUE constraint warning; no production fix attempted.
- 09:06–09:07 — Submitted repository summary. Grouped exploration already exists. Ctrl+O expands output globally and fills the viewport with file contents; collapsing restores concise group rows. Status appears in live turn row and footer. Persistent warning occupies transcript-adjacent space.
- 09:07 — Launched same binary against a disposable committed Python fixture at `/private/tmp/astra-forge-fixture`; accepted trust for this fixture only. Its task strip works. No Forge source edits authorized.
- 09:07–09:08 — Opened F4 during streaming. Picker has model, provider, source/account columns; three Luna routes remain distinguishable. Active model/effort are repeated in the bottom detail line. Selected row is distinct from `current`.
- 09:08 — Submitted actual fixture fix with a three-step plan, patch and unittest verification. Started larger read-only architecture/performance investigation in baseline session.
- 09:08–09:09 — Resized baseline to 80×24 and 100×30. Files hides. Plan summary pins at top. Old `Answered in 64s` metadata remains visible among newer activity; current working state is elsewhere. Narrow footer shortens model to `gpt-…-luna` and working label to `run`.
- 09:09 — Fixture modified formatter and tests, ran unittest successfully, and updated plan to 3/3. Completed steps retain multiline tool metadata; verification step metadata abbreviates command to `exec_command python3 -m, git`.

## Reference sessions

- Codex CLI 0.153.4: successful read-only summary and investigation. Exploration grouped; shell output preview elides middle lines with a transcript hint. Completed response separated by a horizontal rule. Intentional `exit 1` followed by `printf RECOVERED` completed; the failure row says `Ran` with no output, so status is not obvious until final prose. `update_plan` was unavailable in this installed session: its plain-text list is not evidence of a structured plan widget. Model selector inspected at 120×35 and 80×24.
- Claude Code 2.1.251: startup, subscription-access error, model picker, and narrow layout observed. Successful model tasks blocked by organization subscription policy. Do not claim live tool/approval/plan success from this run.
- OpenCode 1.18.23: initial OpenRouter credit failure, then successful OpenAI GPT-5.6 Luna task after selecting an existing route (user explicitly endorsed Luna). Read/search rows compact. Thought labels accumulate separately. Todo updates render repeated lists. Long task and resize inspected.

Official reference documentation consulted: [Codex CLI](https://learn.chatgpt.com/docs/codex/cli), [Claude interactive mode](https://code.claude.com/docs/en/interactive-mode), [OpenCode TUI](https://opencode.ai/docs/tui/). Documentation is supplementary, not evidence of local execution.

## Duration and coverage closeout

The real Forge PTY study totaled **30m52s**: 09:05:55–09:34:44 UTC and 14:22:51–14:24:54 UTC. The pause between windows, release build, source inspection, prototype work, and competitor-only time are excluded. Forge was exercised at 80×18, 80×24, 100×30, 120×35, 160×45, and 220×55. Scenarios included fresh start, summary, tool-heavy investigation, committed-fixture edit and verification, plan updates, approval and denial, failure/recovery, cancellation, editor modes and conflicts, terminal focus, Files search/no-results, model/theme/provider selectors, Review Changes, long conversation, and a three-minute polled command.
