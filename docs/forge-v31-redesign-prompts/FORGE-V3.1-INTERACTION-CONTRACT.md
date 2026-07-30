# Forge V3.1 Interaction Contract

**Owners:** Elena Park — Product & UX; Arjun Mehta — Technology & Engineering  
**Status:** Implemented contract for the current V3.1 shell

---

## 1. Product Contract

Forge uses **one dominant workspace**.

The workspace shows the user’s current activity rather than every capability Forge provides.

Supported workspace views:

```text
Conversation
File(path)
Diff(context)
Run(run_id)
```

Supporting surfaces are independent of workspace view:

```text
Files
    Open(width)
    Closed

Overlay
    None
    Approval(request)
    Confirmation(action)
    Error(details)
```

Core principles:

1. Conversation is Home.
2. File, Diff, and Run are contextual views.
3. Files visibility is user-controlled and independent of workspace view.
4. Keyboard access is complete.
5. Mouse interaction accelerates visible actions.
6. Background activity never changes the current view automatically.
7. Each piece of state has one primary UI owner.
8. Permanent shortcut strips, mystery icon rails, and duplicated status are prohibited.
9. Themes and decorative polish are deferred.

---

## 2. Corrected Default Frames

These are behavioural wireframes, not fixed layout specifications.

### 2.1 Conversation — Default Home

```text
forge / main*                                      5 changes · Review

Conversation

You
Add validation for the port setting.

Forge
I updated the validation and added a clear error.

[Contextual activity summary appears only when actionable]

Ask Forge anything…
```

Rules:

- Conversation remains visible while the agent thinks or a command runs.
- Files may be open or closed according to the user’s preference.
- Only one prioritised activity summary is shown.
- No permanent Chat, Editor, or Diff tabs.
- No icon-only navigation rail.
- No permanent shortcut footer.

### 2.2 File Open

```text
forge / main*                         src/ui/settings.rs · Saved

[Files, only if currently open]       File content
                                      ...

Back                                  Review changes, when relevant
```

Rules:

- Users enter File view by opening a specific file.
- There is no normal full-screen “No file selected” destination.
- Opening a file does not change Files visibility.
- Opening another file while already in File view replaces the current File view.
- Opening Diff from File pushes Diff onto navigation history.

### 2.3 Review Changes

```text
forge / main*                                    2 of 5 changes

[Files, only if currently open]       Diff content
                                      ...

Back          Previous          Next          Open file
```

Rules:

- Diff appears only when explicitly opened.
- A stale Diff cannot be applied until refreshed.
- Change-review language should describe user intent: “Review changes”.
- File lists, summaries, or supporting detail are contextual—not permanent side panels.

### 2.4 Run Active in Background

Conversation remains the current workspace:

```text
forge / main*                                      Running: cargo test

Conversation
...

Running in background · cargo test                View output

Ask Forge anything…
```

Rules:

- Run start does not navigate.
- Agent thinking does not replace Conversation with a full-screen spinner.
- The user may continue reading and composing.
- Selecting **View output** pushes the Run view.

### 2.5 Run View — Explicitly Opened

```text
forge / main*                                      Running

Run: cargo test

live output...

Cancel                                            Back
```

Rules:

- Run becomes primary only after explicit user navigation.
- Cancel is visible while active.
- Leaving Run does not cancel it.

### 2.6 Run Failure — Explicitly Inspected

```text
forge / main*                                      Failed

Run: cargo test

failure output...

Rerun          Open file          View trace       Back
```

Rules:

- Background failure updates the activity summary.
- Failure never auto-navigates.
- Selecting **Inspect failure** opens this view.
- Spawn failure and process exit failure are shown as different states.

### 2.7 Approval Overlay

```text
Approval required

Mode
Direct

Executable
rg

Arguments
["--hidden", "TODO", "src/"]

Working directory
~/project

Environment
inherited

Source
Agent suggestion

Reason
Read project files to locate TODOs.

[Allow once]  [Deny]

Optional:
Remember this exact Direct invocation in this workspace
for the remainder of this Forge session.
```

Rules:

- Approval is a blocking overlay, not a workspace view.
- The default focused action is **Allow once**.
- Clicking outside does nothing.
- `Esc` denies or safely closes; it never approves.
- Mouse events cannot reach the interface beneath the overlay.
- Shell-mode invocations are always **Allow once** and cannot be remembered.

### 2.8 Narrow Layout — 80×24

Priority order:

1. Current workspace.
2. Critical status or approval.
3. Primary action.
4. Contextual navigation.
5. Supporting panes.

Rules:

- Files auto-collapse when space is insufficient.
- Workspace content uses the full available area.
- Approval remains fully usable.
- Long paths are truncated visually but remain inspectable.
- No permanent shortcut footer is introduced to compensate for reduced width.

---

## 3. Navigation Contract

### 3.1 Canonical navigation operations

```text
PushView(view)
ReplaceView(view)
Back
Home
OpenFile(path)
ReviewChanges(context)
OpenRun(run_id)
ToggleFiles
CloseOverlay
```

### 3.2 Push versus Replace

Use `PushView` when the user opens a new kind of contextual activity:

```text
Conversation → File
File → Diff
Conversation → Run
Diff → File
```

Use `ReplaceView` when the user changes the resource inside the current activity:

```text
File(a) → File(b)
Diff(change_a) → Diff(change_b)
Run(run_a) → Run(run_b)
```

### 3.3 Back

`Back` returns to the previous valid workspace view.

It skips transient overlays, stale invalid views, and background state updates.

If no valid previous view exists, Back returns to Conversation.

### 3.4 Home

`Home` always returns to Conversation.

It does not cancel active runs, dismiss required approvals, or alter Files visibility.

### 3.5 Files visibility

Files visibility is:

- Independent of workspace view.
- Remembered per repository.
- Temporarily auto-collapsed when width is insufficient.
- Restored when width returns, unless the user explicitly closed it.
- Accessible through a labelled visible control where space permits, keyboard command, and command palette.

No mystery icon rail is used.

---

## 4. Canonical Semantic Commands

Input methods must emit semantic commands rather than mutate state directly.

### Workspace

```text
GoHome
GoBack
ToggleFiles
OpenFile(path)
ReviewChanges(context)
OpenRun(run_id)
CloseCurrentView
```

### Files

```text
SelectEntry(path)
ToggleDirectory(path)
OpenSelectedEntry
CreateFile(parent)
CreateDirectory(parent)
BeginRename(path)
RequestDelete(path)
RefreshFiles
```

### Conversation

```text
FocusComposer
SubmitMessage
InsertComposerNewline
OpenSlashCommands
OpenGlobalCommandPalette
```

### Diff

```text
SelectPreviousChange
SelectNextChange
OpenDiffFile
RefreshDiff
ApplyChange
```

### Run

```text
CancelRun(run_id)
Rerun(run_id)
EditAndRerun(run_id)
InspectRun(run_id)
OpenRunSource(run_id)
```

### Overlay

```text
ApproveOnce(request_id)
ApproveRememberedDirectInvocation(request_id)
DenyApproval(request_id)
ConfirmAction(action_id)
CancelAction(action_id)
```

### Mouse equivalence examples

```text
Enter on selected file
Double-click file
Command palette: Open File
    → OpenFile(path)
```

```text
Click “Review changes”
Keyboard command: Review changes
Command palette: Review changes
    → ReviewChanges(context)
```

Not every keyboard command requires a permanent clickable control.

Visible actionable objects should support direct mouse interaction.

---

## 5. Command Discovery

Forge has two distinct command systems:

### Slash commands

```text
/
```

- Composer-local.
- Available while writing a prompt.
- Inserts or invokes conversation-oriented commands.

### Global command palette

```text
Ctrl+K
```

- Application-wide.
- Exposes navigation, view, Files, Run, and other semantic commands.

Do not label both simply as “Commands” in the same context.

Bare printable global bindings are prohibited because they conflict with type-to-compose.

---

## 6. Mouse Contract

Supported:

- Left-click pane to focus.
- Left-click row to select.
- Double-click file to open.
- Double-click folder row to toggle.
- Single-click folder chevron to toggle.
- Single-click visible controls and buttons.
- Click activity summaries to inspect.
- Wheel scrolls the pane beneath the pointer without changing keyboard focus.
- Click approval and confirmation actions.

Not supported in V3:

- Right-click menus.
- Drag-and-drop.
- Pane resizing.
- In-application text selection.
- Multi-selection.
- Hover-only actions.
- Mouse-only functionality.
- Invisible click targets.

Double-click is recognised only when:

- Both clicks use the left button.
- Both resolve to the same semantic target.
- They occur within the configured internal threshold.
- No scroll, resize, modal change, or target invalidation occurs between them.

Explicit controls activate at most once when double-clicked.

---

## 7. Asynchronous Event Rules

Asynchronous events update state and activity summaries. They do not issue navigation commands.

### Run started

```text
RunStarted
→ create/update Run state
→ update activity summary
→ do not navigate
```

### Run completed successfully

```text
RunSucceeded
→ update Run record
→ update activity summary only when useful
→ do not navigate
```

### Run failed

```text
RunFailed
→ update Run record
→ show attention-level activity summary
→ do not navigate
```

### Agent thinking

```text
AgentThinking
→ retain Conversation and Composer
→ show non-blocking status
→ do not navigate
```

### Agent response streaming

```text
AgentStreaming
→ append to Conversation
→ retain user-controlled workspace
→ do not navigate away from File, Diff, or Run
```

### Approval required

```text
ApprovalRequired
→ open blocking overlay
→ preserve underlying workspace
→ prevent underlying input
```

### File or Git state changed

```text
RepositoryChanged
→ update relevant models
→ preserve current view where safe
→ surface stale/unavailable states when required
```

---

## 8. Activity Summary Priority

Conversation shows at most one compact activity summary.

Priority:

1. Approval required — blocking overlay, not merely a summary.
2. Run failed — attention.
3. Run active — informational.
4. Changes available — actionable.
5. Agent planning/thinking — informational.
6. Idle — nothing displayed.

Rules:

- Lower-priority activity remains available in an on-demand Activity view.
- The summary never grows into a stack of cards.
- Activity text should describe intent:
  - `5 files changed · Review`
  - `Run failed · Inspect`
  - `Running cargo test · View output`

---

## 9. Approval Scope

### Eligible for remembered approval

Only structured `Direct` invocations may be remembered.

The remembered identity includes:

```text
executable
argument vector
working directory
environment delta
workspace identity
current Forge session
```

The UI wording must be:

```text
Remember this exact Direct invocation in this workspace
for the remainder of this Forge session.
```

### Not eligible

The following are always **Allow once**:

- Shell-mode invocations.
- Commands with ambiguous shell text.
- Commands whose execution identity cannot be represented exactly.
- Destructive actions requiring dedicated confirmation.
- Invocations with approval-sensitive environment values that cannot be safely matched.

### Approval safety

- Default action: Allow once.
- `Esc`: deny or safe close, never approve.
- Click outside: no action.
- Double-click: at most one approval.
- Modal blocks underlying hit targets.
- Approval details include mode, executable/shell, arguments or command string, working directory, environment delta, provenance, and reason.

---

## 10. Edge-State Transitions

### 10.1 Run cancelled

```text
Running
→ user selects Cancel
→ cancellation requested
→ preserve output
→ final state: Cancelled
→ current view remains unchanged unless already in Run
→ activity summary: Run cancelled, briefly or only when relevant
```

Cancellation is distinct from failure.

### 10.2 Process failed to spawn

```text
Run requested
→ process could not start
→ state: SpawnFailure
→ show executable, arguments, working directory, and cause
→ do not display a fabricated exit code
→ offer Edit & Rerun and Back
```

Spawn failure is distinct from a process that exited non-zero.

### 10.3 Network connection lost while streaming

```text
Agent streaming
→ connection lost
→ preserve received content
→ mark response Interrupted
→ keep Composer usable
→ offer Retry or Continue
→ do not replace the current workspace
```

### 10.4 Open file renamed externally

```text
File(path_a) open
→ external rename detected to path_b
→ verify identity when possible
→ update File view to path_b
→ preserve scroll/cursor state
→ show brief “File renamed” notice
```

If identity cannot be established safely, treat it as deletion plus a newly discovered file.

### 10.5 Open file deleted externally

```text
File(path) open
→ deletion detected
→ keep File view
→ show “File no longer exists”
→ preserve any in-memory content
→ offer Back and Locate
→ never silently switch to another file
```

### 10.6 Diff becomes stale

```text
Diff open
→ repository changes affect reviewed content
→ mark Diff stale
→ preserve current review position
→ disable Apply
→ show “Changes updated”
→ require Refresh before Apply
```

### 10.7 Approval at 80×24

```text
Approval required
→ overlay uses full available workspace
→ show essential fields first:
   mode, command, directory, reason
→ remaining detail scrolls
→ Allow once and Deny remain visible
→ remembered approval option appears only if space permits and remains keyboard-accessible
```

### 10.8 Mouse disabled

```text
mouse_capture = false
→ no pointer actions are required
→ all capabilities remain keyboard-accessible
→ no mouse-specific hints occupy space
→ terminal-native text selection remains available
```

Keyboard-only smoke tests must cover the complete primary workflow.

### 10.9 Hit target invalidated after resize or rerender

```text
mouse event received
→ resolve against latest frame generation
→ target missing or stale
→ ignore safely
→ do not activate coordinates from an older frame
```

Scroll, resize, modal changes, and list mutations cancel pending double-click state.

---

## 11. Contextual Hints

There is one application-owned contextual hint slot.

It is shown only when it materially helps the current transient interaction.

Examples:

```text
Enter confirm · Esc cancel
```

```text
Tab move · Enter allow once · Esc deny
```

Ordinary navigation does not display a permanent shortcut footer.

Help remains available through:

- Global command palette.
- Contextual help command.
- Onboarding.
- Documentation.

---

## 12. Implementation Boundary

The implementation must encode:

- State models.
- Layout policies.
- Semantic commands.
- Navigation transitions.
- Input equivalence.
- Responsive priorities.

It must not encode screenshots as fixed coordinate logic.

Widgets may register semantic hit regions during rendering, but they must not contain business logic for mouse commands.

Required input flow:

```text
Keyboard event ─┐
Mouse event ────┼→ Semantic command → State transition
Palette action ─┘
```

---

## 13. Acceptance Criteria

The contract is satisfied when:

- Conversation remains visible during background Run and Agent Thinking.
- Background events never auto-navigate.
- Files visibility is independent and remembered per repository.
- No permanent Editor or Diff tabs exist.
- No mystery icon rail exists.
- No permanent shortcut footer exists.
- Slash commands and the global command palette have distinct roles.
- Bare printable global keybindings do not conflict with type-to-compose.
- Mouse wheel scrolls the hovered pane without changing keyboard focus.
- Every meaningful capability is keyboard-accessible.
- Mouse actions emit the same semantic commands as keyboard actions.
- Shell-mode approvals cannot be remembered.
- Remembered Direct approvals use exact workspace- and session-scoped identity.
- All nine edge-state transitions behave as defined.
- The interface remains usable at 80×24.
- Themes and decorative polish remain outside this implementation phase.

---

## 14. Decision

This contract supersedes contradictory details shown in earlier V3 and V3.1 visual boards.

Where a mockup and this contract disagree, this contract is authoritative.
