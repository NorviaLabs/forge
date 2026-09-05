# Reference CLI study

## Scope and evidence

Installed versions inspected on 2026-09-05: **Forge 0.1.0-beta.8** for the full scenarios and source-identical **Forge 0.1.0-beta.9** for final-baseline startup verification, **Codex CLI 0.153.4**, **Claude Code 2.1.251**, and **OpenCode 1.18.23**. Dedicated real tmux PTYs were used. These are locally available versions, not a claim that every executable was globally latest. Model intelligence, speed and token efficiency were not benchmarked.

Codex completed a read-only repository summary, investigation and failure/recovery. Its installed session reported `update_plan` unavailable; a plain list is not a structured plan-widget test. OpenCode initially failed on OpenRouter credits, then completed summary and a three-minute todo/tool task using the existing **OpenAI GPT-5.6 Luna** route, as the user explicitly endorsed. A real external-directory approval was exercised. Claude's organization disabled subscription access, so its live study covers startup, error, composer, model selector and narrow layout; successful tool/plan/approval behavior is **unobserved**.

Supplementary official documentation describes Codex's terminal operation and configuration ([Codex CLI](https://learn.chatgpt.com/docs/codex/cli)); Claude's transcript detail toggle and task-list controls ([interactive mode](https://code.claude.com/docs/en/interactive-mode)); and OpenCode's TUI command interfaces ([OpenCode TUI](https://opencode.ai/docs/tui/)). These sources support available interaction concepts only, not unobserved rendering or local account access. The comparisons below are local observations unless marked otherwise.

## Representative comparison

| Area | Forge observed | Codex observed | Claude Code observed | OpenCode observed |
|---|---|---|---|---|
| Startup | Files/search + identity/help content + bordered composer; task strip where available. | Compact startup identity box above a full-width conversation/input flow. | Small brand/model/workdir cluster, large quiet transcript area. | Centered wordmark and composer, sparse hints. |
| Conversation | Rich answer structure, strong indentation; width sharply reduces with editor. | Plain full-width response, hanging list indent and clear final separation. | Task blocked; error becomes a transcript result. | Left-rail user prompts, wide text area, compact role/timing line. |
| Streaming | Live phase/timer plus footer state; intermediate summaries and thought labels accumulate. | One `Working` line with elapsed and interruption hint near input. | Successful stream unavailable. | Live thinking label and footer activity indicator; past thought summaries accumulate. |
| Tools | Existing exploration group; Ctrl+O exposes large payloads; shell sessions/polls separate. | `Explored` group for reads/searches; shell output preview with omitted-line count/detail hint. | Unobserved; docs describe detailed transcript access. | Read/search rows usually one line; shell output has a local rail block. |
| Plans | Counts, active item, completed metadata, pinned summary and `/plan`. | Plain-text plan only; structured tool unavailable. | Unobserved; docs describe task-list toggle. | Actual todo tool renders successive full checklist updates. |
| Approvals | Inline action/reason/cwd/choices; several waiting signals; reason can truncate. | Real explicit command approval showed environment, reason, command and three choices; approved once and completed. | Unobserved because model access blocked. | Input-area permission panel shows external-directory scope/pattern and Allow once/Always/Reject. |
| Errors | Exit 1 and exit 0 both summarized as “exited”; final prose clarifies. | Intentional exit 1 row says `Ran`, no output; final prose clarifies. | Clear subscription-access diagnosis, then a whimsical “Baked” completion label even though task failed. | Initial credit failure displayed prominently as a rail block, remains in history. |
| Completion | `Answered in …` telemetry; prior completion can appear under later activity. | Rule separates final response; working line removed. | Blocked task still has completion phrase/time; don't copy that semantic ambiguity. | Final response followed by model/duration; prior tools remain above. |
| Composer | Thick full border focused; multiline growth good; terminal can crowd it out at 80×24. | Simple prompt marker and modest status line. | Two horizontal rules delimit input; effort/status sits nearby. | Left rail and bottom rule make input a distinct region; narrow layout retained. |
| Status | Model/effort/context/token/cache footer, phase elsewhere, task strip separate. | Model/workdir near input; rate-limit warnings may interrupt transcript. | Model/effort and permission mode visible; blocked account explicit. | Model/agent route in composer; context and active indicator in footer. |
| Responsive | Files hides; editor/chat ratio harms narrow chat; very wide prose unbounded. | 80-column model descriptions wrap beneath names; older scrollback reflows imperfectly immediately after resize. | Narrow startup/error remains readable; only limited states exercised. | 80-column task view retains input/status; tool lines wrap. |

## Patterns to reinterpret

| Pattern | Best observed reference | Problem solved / why it works | Applicability | Forge adaptation |
|---|---|---|---|---|
| Aggregate exploration | Codex | Many reads become a meaningful activity label, reducing repetitive verbs. | High; Forge already supports this. | Refine existing groups; preserve exact calls through Ctrl+O. Do not invent task summaries. |
| Compact routine tool rows | OpenCode | File target remains visible without dumping content. | High. | State + kind + target; raw output only in active preview/failure/detail. |
| One live status near input | Codex | Eyes do not chase a moving log to know whether work continues. | High. | Existing live row owns phase/time; footer retains configuration/context. |
| Permission at intervention point | OpenCode | Decision displaces routine input and displays scope beside choices. | High, presentation only. | Keep Forge inline approval and its exact options; consolidate waiting indicators. No Always Allow addition. |
| Simple input boundary | Claude | Horizontal rules distinguish input without enclosing every region. | High; observed even without model access. | Composer top rule, shared text origin, explicit blue owner marker. |
| Route distinctions | Forge | Model identity is not provider/account route identity. | Essential; Forge is stronger here than a simple flat list. | Retain model/provider/source-account columns and active route detail. |
| Final response separation | Codex | Clear transition from work to result. | High. | Compact completed activity, then dominant answer and its own metadata; avoid full-width decorative rules per message. |
| Task checklist | Forge/OpenCode | Active step explains near-term intent. | High, but neither historical form is ideal. | Latest plan in place; old versions/details retained under existing disclosure. |
| Restrained typography | Codex/Claude | Content hierarchy relies on alignment, spacing and limited weight. | High. | Sentence-case headings, fixed cell spacing, mostly neutral text. |
| Reasoning summaries | OpenCode, with caveat | Short titles expose a reasoning phase without full raw text. | Conditional. | Preserve current visibility setting; avoid accumulating empty or routine headings in compact completed turns. |

## Patterns explicitly rejected

Do not import competitor modes, permission policies, favorite models, agents, commands, session controls, free-tier badges or background-task workflows. Do not copy OpenCode's repeated todo blocks, large startup wordmark, animated footer meter or rail on every message. Do not copy whimsical completion verbs or label a blocked request as successful. Do not copy Codex's ambiguity for no-output failed commands. Forge's identity is the coherent combination of delegation, direct editing, terminal inspection and review, with one keyboard owner—not visual imitation of a single reference.

## Limits

Claude streaming/tools/plans/approvals require an account with working access to validate later. Codex structured planning was not available in this session. Reference captures are selected text snapshots, not complete video traces. Exact contrast and font rendering of competitor UIs were not measured. Findings about Forge were independently observed with its freshly built binary; competitor model statements about Forge's code are not used as architectural evidence.
