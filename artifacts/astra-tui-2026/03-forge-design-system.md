# Forge design system — 2026 proposal

Baseline `dfa46a07aa43551395ba7cb6b4867ab2d8a9c324`. This is a presentation proposal, not a change to the current interaction contract. Existing `FORGE-DESIGN.md` remains the runtime contract until implementation lands. This proposal deliberately supersedes its full pane boxes, violet narration, green-tinted neutrals and 95% frame inset; it retains its focus, outcome, input ownership and reachable-binding invariants.

## Principles

1. **Show the work's state, not the transport's state.** A running shell session remains “Running command” during `write_stdin` polling. Never claim progress unsupported by an event.
2. **A turn has one owner and one outcome.** User request, activity, answer and timing belong to the same turn. An old completion must never sit beneath a newer live action.
3. **Working detail expands; completed detail recedes.** Preserve exact commands, outputs, narration, plan revisions and errors through the existing detail toggle. Do not replace them with model-generated summaries.
4. **Focus is structural; state is local.** One blue `>` title marker identifies the effective keyboard owner. Orange marks active work inside that region or elsewhere. Outcome never recolors the focus frame.
5. **Spend cells on evidence.** A search field needs a row, not a box inside a box. Do not reserve blank space for absent metadata.
6. **Preserve human control.** Approval and agent questions outrank motion; editor buffers, terminal lifetime, queues, task switching and cancellation retain their existing semantics.
7. **History is readable without trusting an inference.** “Finished” means the turn ended; it does not imply tests passed. A later successful command does not erase an earlier failure.

## Semantic tokens

Keep `forge_config::ThemePalette` as the theme boundary and existing `theme::*` accessors as the renderer entry point. Do not introduce a parallel enum-based theme registry. The names below are existing palette fields except `activity`, a proposed backwards-compatible optional TOML field, resolved into an `Rgb` field in `ThemePalette` and falling back to `warning` for custom themes. That fallback preserves themes, not the preferred built-in hue distinction.

| Palette field | Dark | Light | Meaning |
|---|---|---|---|
| `background_deep` | `#080808` | `#E8E8E8` | outer ground and separators |
| `background` | `#141414` | `#FAFAFA` | content canvas |
| `surface` | `#1E1E1E` | `#F0F0F0` | composer, secondary surfaces |
| `surface_raised` | `#1E1E1E` | `#FFFFFF` | modal surface |
| `surface_hover` | `#292929` | `#E2E2E2` | selected-row ground |
| `selection` | `#292929` | `#E2E2E2` | selected-row ground, not focus |
| `border` | `#343434` | `#BDBDBD` | meaningful boundaries |
| `border_muted` | `#292929` | `#D8D8D8` | quiet separators |
| `text_primary` | `#EBEBEB` | `#202020` | durable content |
| `text_secondary` | `#A0A0A0` | `#545454` | supporting content and metadata |
| `text_muted` | `#757575` | `#737373` | optional hints only; never sole critical text |
| `accent` | `#439EFD` | `#005EB8` | keyboard focus and interaction |
| `accent_soft` | `#182B3D` | `#DFECF8` | rare focused-selection ground |
| `activity` (new optional token) | `#FFA31D` | `#965300` | active work only |
| `info` | `#1DB0BB` | `#00717B` | references and informational structure |
| `structure` | `#A0A0A0` | `#545454` | response headings/list markers |
| `agent` | `#A0A0A0` | `#545454` | narration; neutral rather than a second brand accent |
| `success` | `#5BDB87` | `#18723B` | confirmed successful result glyph |
| `warning` | `#E6C66A` | `#795B00` | uncertainty and approval attention |
| `error` | `#F26D78` | `#B52C42` | failed result / destructive consequence |
| `waiting_border` | `#E6C66A` | `#795B00` | local waiting indicator, not keyboard focus |
| `cursor` | `#439EFD` | `#005EB8` | owned caret; hide inactive painted caret |
| `tag` | `#A0A0A0` | `#545454` | neutral labels |
| `search_match` | `#E6C66A` | `#795B00` | matched text; active match also underlined |
| `diff_add` | `#5BDB87` | `#18723B` | `+` gutter and changed text |
| `diff_remove` | `#F26D78` | `#B52C42` | `-` gutter and changed text |
| `scan_band` | `#141414` | `#FAFAFA` | neutralize permanent list backgrounds |
| `zebra_row` | `#1A1A1A` | `#F0F0F0` | table rows only |

Retain syntax token fields and tokenization. Remap comments to secondary, keywords to restrained blue, strings to green, numbers to cyan; never dim whole code blocks. Terminal application output retains its ANSI colors; do not recolor shell output to enforce this palette. Theme `system`, drop-ins, live preview, cancel restoration and theme persistence remain unchanged. Validate the existing ≥60° accent/outcome hue separation invariant after rounding RGB values.

No `DIM` modifier on meaningful content. No saturated backgrounds for whole turns, completed plans or panes. Important metadata uses secondary, not muted. Glyph plus text conveys every state in monochrome.

## Typography in terminal cells

All content uses the user's monospace font at one size and line height. HTML uses a fixed cell model only to preview it.

| Role | Treatment |
|---|---|
| Pane title | sentence case, primary; focused title bold blue, prefixed `>` |
| User request | primary, 1-column left rail, no full box |
| Final answer | primary; headings bold, sentence case, no uppercase conversion |
| Narration | secondary; exact text retained, no italic requirement |
| Live action | bold primary label with orange `[>]`; elapsed secondary |
| Tool summary | secondary; exact target primary when running/failed |
| Paths/references | info, underline only when currently focusable under existing behavior |
| Key hint | secondary key, muted verb; one space, pairs separated by ` · ` |
| Code | primary plus existing syntax spans; no mandatory background band |
| Failure | red state marker plus primary diagnosis; full red paragraphs prohibited |

## Cell metrics

Proposed shared constants live in the TUI presentation layer, not config:

| Constant | Columns / rows |
|---|---|
| `FRAME_INSET_X` | 1 column each side |
| `PANE_PAD_X` | 1 column each side; no extra padding from a removed border |
| `PANE_TITLE_H` | 1 row |
| `PANE_SEPARATOR_W` | 1 column |
| `TRANSCRIPT_MAX_W` | 88 content columns; left aligned within chat |
| `TURN_GAP_H` | 1 blank row between completed turns |
| `REQUEST_GAP_H` | 1 row after request |
| `ACTIVITY_GAP_H` | 0 between related tools; 1 across narrative/approval boundaries |
| `PLAN_ITEM_GAP_H` | 0; continuation aligns after 4-column state prefix |
| `PLAN_META_INDENT` | 4 columns; expanded detail only for completed items |
| `COMPOSER_PAD_X` | 1 column; same text origin as chat |
| `COMPOSER_RULE_H` | 1 top rule; no side or bottom border |
| `COMPOSER_PAD_Y` | 0 rows; input content 1–10 rows, height-limited below |
| `FOOTER_H` | 1 row; no separate separator row |
| `MODAL_PAD_X` | 2 columns inside 1-column border |
| `MODAL_PAD_Y` | 1 row inside border; omit at height <24 |
| `MODAL_SECTION_GAP_H` | 1 row |
| `TREE_INDENT_W` | 2 columns per nesting level |

## Border audit and replacement rules

| Existing boundary | Proposed rule | Purpose |
|---|---|---|
| Files outer box | remove; shared vertical separator at right | separate tree from content |
| Files search inner box + separator | replace with one surface row | identify text input without losing four rows |
| Workspace box | remove sides/bottom; title row plus shared dividers | content ownership |
| Conversation box | remove; title row and request rail | chronology and pane identity |
| Composer full box | one top rule with focus marker | input ownership and transcript boundary |
| Footer top rule | remove | footer surface already separates chrome |
| Tool output | 1-column left rail in expanded detail only | connect payload to call |
| Plan | no border | structured list, not a separate application |
| Approval | local warning rail, blue focused title | preserve inline approval focus block |
| Modal | one single-line box | temporary exclusive keyboard owner |
| Terminal | keep one top rule; `>` focus marker | terminal boundary without touching emulator content |
| Diff | no extra box inside workspace | existing diff hierarchy is sufficient |

Never double-draw adjoining separators. Selected rows use neutral ground plus a pointer. Inactive selection retains neutral ground but loses blue. A modal suppresses underlying focus markers; closing restores the actual previous valid owner. Pane titles reflect existing owners, not arbitrary labels copied from legacy enums (`Workspace` displays the file or Review Changes, never CHAT).

## Glyph vocabulary

Use ASCII for mandatory state and kind vocabulary; it occupies predictable cells in Menlo, Monaco, SF Mono, Consolas and JetBrains Mono. Optional box-drawing is decorative structure with `|`, `-`, `+` fallback. No emoji or Nerd Font requirement.

| State | Token | Meaning |
|---|---|---|
| Pending | `[ ]` | existing queued/pending state |
| Active | `[>]` | currently running; orange |
| Complete | `[x]` | completed; neutral in history, green only for confirmed successful result |
| Failed | `[!]` | failure; red marker |
| Cancelled | `[-]` | cancelled; secondary |
| Warning | `[?]` | warning/uncertainty; yellow |
| Blocked | `[|]` | waiting for user; yellow |

Tool kind labels replace decorative icons: `Read`, `Search`, `Shell`, `Git`, `Edit`, `Check`, `Web`, `Plan`. Expanded rows reserve 6 columns for the label. Compact glyph equivalents are `R`, `?`, `$`, `G`, `~`, `T`, `@`, `#`, always followed by a readable label when displayed independently. Use actual tool identity when classification is uncertain; do not call a shell command “Check” merely because it contains “test”.

Tree expansion uses `>` / `v`; selection pointer `>`; dirty editor uses `*`; Git retains existing status data, displayed as `M A D ? ! U` with existing semantics. Optional Enter symbol `⏎` is a hint only; fallback `Enter`. No key event mapping depends on a glyph. Plan failed/skipped markers are reserved vocabulary, **not new plan states**: only render them if an existing input explicitly supports them. Baseline plans support pending/in_progress/completed; cancellation does not fabricate a skipped step.

## Responsive geometry

Let `B = terminal_width - 2`. Keep the enforced 80×18 minimum. Files visibility retains the existing width gate (116) and user visibility state. With no file/diff open, do not allocate an empty editor: conversation occupies the remainder, as observed in the baseline.

When file/diff is open:

1. Files width is 22 at 116–159, 24 at 160–199, 28 at ≥200; zero below 116 or when hidden.
2. Subtract one separator per adjacent visible pane from B.
3. Chat width is `min(88, max(44, round(remaining * .55)))`; workspace gets the rest. If workspace would fall below 32, reduce chat to leave 32. Padding is inside these widths.
4. No focus-triggered resizing. Opening/closing an existing pane or resizing the terminal are the only geometry triggers.

| Frame | Files | Workspace | Chat | Outer + separators |
|---|---:|---:|---:|---:|
| 80×24 | 0 | 33 | 44 | 3 |
| 100×30 | 0 | 44 | 53 | 3 |
| 120×35 | 22 | 42 | 52 | 4 |
| 160×45 | 24 | 59 | 73 | 4 |
| 220×55 | 28 | 100 | 88 | 4 |

Widths include pane padding. At very wide sizes, prose wraps at 88 columns, code/diffs/terminal use available width. Long paths use middle ellipsis with filename preserved only where the baseline already truncates; selected model route detail and approval command must remain fully inspectable. Never mutate stored values.

Height budget: header 1; existing task strip 1 only when present; footer 1. Composer input is `min(10, requested_visual_lines, max(1, floor(available_body_h/3)))` plus top rule. If terminal open, allocate `min(12, floor(available_body_h * .35))` rows including its rule, then reduce that allocation until chat retains title + 4 transcript rows + composer. Minimum terminal allocation is 4; at 80×18 preserve at least 2 transcript rows. Do not allow terminal to erase composer, as observed at 80×24. Pane bottoms share the body bottom; editor mode row aligns with the final composer row. Terminal is below the entire body, footer below terminal.

Modals: width `min(84, W-4)`, compact confirmations `min(64,W-4)`; height from actual content, capped at H-4. Existing scrolling retains overflow content. Model picker columns are 3-pointer + flexible name + 2 gap + 20 provider + 2 gap + 15 source/account; selected detail wraps beneath list before sacrificing required identity. Reserve title/filter/column labels/detail/hints before list rows. Theme remains the existing bottom dock (up to 12 rows); do not turn it into a new centered workflow.

Source reconciliation: `files_fit` currently applies the 110-content-column threshold after the 95% inset, producing a 116-frame-column gate. When replacing the inset, explicitly preserve the 116-frame-column visibility contract rather than accidentally lowering the gate. Baseline terminal docks under Files/workspace only when an editor is open; this proposal intentionally moves the same terminal pane below the shared body as a geometry change, without changing its lifecycle or commands. Mandatory glyph tests passed in Menlo/Monaco/Courier; use ASCII Enter because the optional return symbol falls back in two fonts.
