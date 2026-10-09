# Bring the navigator closer to the horizontal-strip proposal

Status: implemented locally on `fix/header-tab-focus`; changes remain uncommitted.

Reference: `artifacts/consolidated-navigator-proposal.png` (Image #1), compared
with the supplied current Forge crop (Image #2). Source inspected on
`fix/header-tab-focus`, including its existing uncommitted changes, 2026-10-08.
The crop does not establish the full terminal size, session count, or behavior.

## Comparison

| Element | Proposal | Current evidence | Action |
| --- | --- | --- | --- |
| Global session strip | One row across the work surface, above local navigation | Already present; the crop shows one underlined `header-tab-focus` chip. `layout.rs` allocates the strip before splitting columns. | Keep the architecture. Verify multiple sessions and overflow rather than rebuilding it. |
| Strip-to-navbar gap | Exactly one blank row | Appears present in the crop; layout already reserves a chrome gap when the strip is visible. | Preserve and verify at compact heights. |
| Navbar-to-search spacing | Search begins on the next row | A rounded file-pane top border occupies the intervening row. | Remove the redundant enclosing explorer frame in navigator composition. |
| Navigator outline | Flat content surface with a quiet right divider | Current explorer has a rounded box, including a top rule and left rail. | Use a shared inset content rectangle and a neutral right divider, avoiding doubled seams. |
| Left alignment | Navbar tiles and search surface share a one-cell inset | Navbar and framed explorer interior have different origins; search text adds the composer's two-cell inset. | Define one navigator content origin; place search text one cell inside its field. Keep tree hierarchy indentation intentional. |
| Tab segments | Distinct Sessions, +, Files, Git tiles with consistent gutters | Current row stretches labels across the pane; selected Files is visible, inactive segments blend into the row. Geometry reserves boundary cells inside tab rectangles. | Make one-cell gutters explicit in shared tab/action geometry and use quiet inactive tile fills. Keep widths responsive. |
| Selected tab versus keyboard owner | Unfocused Files stays selected with neutral bold text; focused control adds `>` and cursor emphasis | Current selected Files text is accent blue even while Conversation owns input. | Reserve accent/cursor emphasis for the actual owner; retain the selected tile fill and bold label when unfocused. |
| Session selection versus cursor | Open chip remains marked while cursor can move to another chip | Existing `TaskStrip` already separates `selected` and `focused`, with underline, fill, and `>`. The single-session crop cannot demonstrate independent movement. | Preserve this behavior and capture both proposed states. |
| Long labels and overflow | Truncation, one-cell gaps, `+N more`, no wrapping | Existing strip caps widths, uses cell-width truncation, and computes hidden count. Not demonstrated by the crop. | Validate with enough sessions to overflow, including Unicode labels. |
| Visible chip name | Human-readable session title | `header-tab-focus` looks technical, but rendering uses `task.label`, not the branch field. | Do not hide it as a debug label or hardcode the proposal's titles. Use named sessions for comparison; investigate title generation separately only if incorrect. |

The missing `>` on Files in Image #2 is not itself a bug: `> Conversation`
identifies the current keyboard owner. The two proposal panels depict different
focus states and must be recreated before comparing focus presentation.

## Implementation sequence

### 1. Flatten the navigator body and align its geometry

Primary files:

- `crates/forge-tui/src/app/render.rs`
- `crates/forge-tui/src/file_explorer.rs`
- `crates/forge-tui/src/app/mouse.rs`

Keep the global strip outside the navigator. Keep the one-row navbar. Make the
search surface start at `tabs.bottom()` with no border or spacer row between.
Use one cell of left inset for the navbar and navigator body. Search text gets
one additional cell inside the field; tree disclosure/selection markers share
that text origin before depth indentation.

Inspect every `FileExplorerWidget` caller before changing its frame contract:
Files and Git must remain coherent. Reuse existing rectangle helpers where
possible; if framed callers still need the old treatment, make composition
explicit rather than globally removing all frames. Apply the same navigator
shell treatment to the Sessions body without changing its list behavior.

Remove frame-dependent title decoration for Files/Search and preserve a
visible non-colour owner signal in the field/tree's reserved marker space.
Keep the selected-path footer, result count, search caret, scrolling, and
empty/loading/error states. Update mouse search/tree offsets together with
painting so removing the border cannot shift click targets by a row or cell.
Draw only the necessary right divider between navigator and conversation;
handle the temporary full-width navigator without a redundant outer box.

### 2. Tighten navbar spacing and owner styling

Primary file: `crates/forge-tui/src/widgets/navigator.rs`.

Use `navigator_tab_rects` and `new_session_cell` as the common geometry source
for painting and pointer routing. Reserve one-cell gaps and a stable focus
marker slot, including for `+`. Account for the gaps before distributing
remaining width. Do not let gutters trigger an unrelated tab through the
current nearest-tab fallback; define and test the intended gap hit behavior.

Selected tab: accent-soft ground plus bold primary text. Keyboard cursor:
`>` plus neutral cursor fill, with accent on the marker as appropriate.
Inactive tab: secondary text on a quiet tile ground. Hover must not move
labels or imply keyboard ownership. Retain existing palette tokens; do not
retune the global theme to match screenshot pixels.

### 3. Preserve the existing strip and focus model

Review, changing only if validation finds a mismatch:

- `crates/forge-tui/src/widgets/task_strip.rs`
- `crates/forge-tui/src/layout.rs`
- `crates/forge-tui/src/app/focus.rs`
- `crates/forge-tui/src/app/input.rs`
- `crates/forge-tui/src/app/render.rs`

Reuse the existing `selected`/`focused` split and shared chip rectangles.
Retain one-row chips, reserved marker slots, equal padding, truncation, and
`+N more`. Use terminal underline as the quiet bottom marker; do not spend a
second row imitating a pixel underline. If the focused chip pushes the open
chip outside the visible window, preserve truthful open-session identity in
the conversation; do not make the cursor look like the newly opened session.

Keep keyboard ownership exclusive across navbar, horizontal strip, vertical
Sessions list, search, tree, and Conversation. Moving the strip cursor alone
must not switch the conversation or local navigator tab. Preserve existing
Enter/double-click activation, navigation return paths, drafts, and scroll.
Keep the vertical Sessions list and its attention/lifecycle information.

### 4. Reconcile the design reference

Update affected sections of `FORGE-DESIGN.md` once implemented and verified.
Its current strip description already matches much of this proposal, while
§7.7/§11 still contain descriptions of the historical list-only choice.
Resolve that inconsistency, and document the new border/inset treatment without
claiming unverified usability improvements. Do not add files under `docs/` to git.

## Acceptance and validation

Capture the same sessions and terminal dimensions before and after:

1. Navbar owns input on Files: one `>` at Files; open session stays marked;
   search sits immediately under the navbar.
2. Strip owns input on a different session: one `>` on that chip; open session
   remains marked; Files stays selected without claiming focus; conversation
   content remains unchanged until activation.
3. Conversation owns input: only its owner marker is shown; selected Files and
   open-session identity remain visible.
4. Repeat with one session, several fitting sessions, overflow, long and wide
   Unicode names, and a pending approval. Verify cursor visibility and hidden
   counts without wrapping or extra title rows.
5. Inspect `80×18`, `120×40`, `160×50`, and both sides of affected responsive
   breakpoints. Include temporary full-width navigation, dark/light themes,
   and no-colour/ASCII fallbacks. Compare cell geometry, not screenshot pixels.
6. Exercise mouse clicks in tabs, `+`, gutters, search, first/last tree rows,
   and strip chips after resize. Check keyboard transitions and preservation
   of drafts, selections, reading positions, and running sessions.

Add or update targeted behavior tests in the existing app test modules
(`mouse.rs`, `workspace.rs`, `multi_task.rs`) for hit mapping, exclusive focus,
and cursor movement versus session activation. Test overflow identity/count
and Unicode boundaries where logic changes. Use terminal captures for visual
spacing and palette review; do not add brittle style/layout snapshot tests.

Run only the relevant `forge-tui` test filters for changed behavior, then
`cargo fmt --all -- --check` and the repository-required Clippy check. Record
actual commands and results at implementation handoff. Never run the full
workspace test suite. This plan-only change requires no Rust build or tests.

## Scope and delivery

Preserve the existing dirty worktree. Before implementation, reconcile this
in-progress feature branch with latest `main` safely; do not switch or overwrite
its unfinished changes. Implement a focused patch and deliver through a feature
branch/PR when publication is requested. No new dependencies, focus framework,
session lifecycle changes, or hardcoded demonstration labels are needed.


## Implementation result — 2026-10-08

- Flattened Files, Sessions, and Git navigator bodies under a shared left inset
  and right divider. Search follows the navbar immediately; the tree starts
  on the following row. Mouse search/tree/Git coordinates follow the new rows,
  and clicking the file footer cannot open an offscreen entry.
- Added real one-cell tab gutters with non-actionable hit regions. The navbar
  uses neutral bold selected labels and a separate cursor tile/marker. The
  minimum navigator width is 32 cells to retain full labels and focus slots.
- Preserved the existing global strip and session switching model. File-tree
  focus uses a reserved marker; content search emits one marker only while
  focused. Updated the design reference and removed the redundant strip-gap
  expression flagged by Clippy.
- Preserved all pre-existing branch changes, including the temporary review
  harness. No dependencies added and no commits or PR created.

Validation: targeted explorer (36), workspace (36), multi-task (58), mouse (51),
plus navigator-filter tests (18); some filters overlap. Formatting and workspace
Clippy with warnings denied passed. A debug CLI build passed. Colour-sensitive
checks run with `NO_COLOR` unset because this execution environment sets it.

Reviewed Ratatui captures in both built-in themes at 80×18, 115×40, 116×40,
120×40, 135×40, 136×40, and 160×50, covering navbar, strip, and conversation
ownership. `navigator-implementation-preview.png` is a rasterized selection
of those fixture buffers, not a screenshot of a user's session. The temporary
capture test was removed, restoring the pre-existing review harness exactly.
Live tmux startup/search/resize checks used the debug binary in a disposable
repository at 120×40, 80×18, and 160×50. These checks establish rendering and
interaction evidence, not a usability study or provider/network validation.
