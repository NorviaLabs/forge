# Forge — Product Requirements Document

**Version:** 1.0 Draft
**Status:** Proposed product direction
**Owner:** Mohit Ranka
**Product:** Forge by Norvia Labs
**Category:** Agent-native terminal development workspace
**Supersedes:** Forge PRD v0.13 as the primary product vision

---

## 1. Executive Summary

Forge is an open-source, terminal-native development workspace where developers and coding agents work together on the same repository.

Forge enables developers to:

* delegate coding tasks to AI agents
* inspect and modify agent-generated code directly
* intervene when an agent encounters ambiguity
* run independent tasks in parallel
* isolate writing agents through Git branches and worktrees
* review diffs, tests, diagnostics, and commands from one workspace
* resume agent execution after manual changes
* choose among hosted and local model providers
* retain control over permissions, cost, context, and integration

Forge is not intended to maximise autonomous code generation at the expense of developer control. Its purpose is to make agent-assisted development reliable, inspectable, and easy to steer.

The product thesis is:

> Coding agents are most useful when developers can delegate substantial work without losing the ability to inspect, intervene, edit, redirect, and safely integrate the result.

Forge combines an agent harness, lightweight development environment, Git worktree manager, and review surface into one terminal application.

---

## 2. Product Vision

### 2.1 Vision statement

> Forge is the open workspace for human-directed software development with coding agents.

Developers should be able to move fluidly between:

* delegating work
* observing progress
* manually editing code
* answering agent questions
* reviewing changes
* running tests
* resolving conflicts
* merging or discarding results

They should not need to reconstruct context across multiple terminals, editors, agent sessions, Git commands, and review tools.

### 2.2 Product promise

> Run coding agents in parallel. Take over when needed. Merge with confidence.

### 2.3 Product positioning

Forge is:

* terminal-native
* open source
* model-agnostic
* human-in-the-loop by design
* Git-aware
* suitable for local repository work
* extensible through tools, MCP servers, skills, and hooks

Forge is not primarily:

* a chatbot
* an autonomous agent swarm
* a general-purpose workflow engine
* a replacement for foundation models
* a complete Vim or Neovim clone
* an enterprise governance platform
* a proprietary model distribution service

---

## 3. Problem

Coding agents can complete increasingly substantial software tasks, but the surrounding developer workflow remains fragmented.

A developer commonly has to manage:

* one or more agent sessions
* several terminal tabs
* an external editor
* manually created Git branches and worktrees
* test processes
* logs and diagnostics
* diff review
* merge conflicts
* model-specific interfaces
* token usage and permissions

The workflow becomes particularly difficult when an agent cannot complete a task autonomously.

Examples include:

* requirements are ambiguous
* the agent chooses an unsuitable implementation
* a small manual correction is faster than another prompt
* several files must be compared
* a merge conflict requires judgment
* tests fail for an environmental reason
* the agent needs domain knowledge from the developer
* generated code is mostly correct but requires local surgery
* multiple agents are modifying related areas

The developer must then leave the agent interface, open the relevant files elsewhere, understand the state again, make changes, return to the agent, and explain what happened.

This produces four core problems.

### 3.1 Context switching

Developers repeatedly switch between the agent, editor, terminal, Git tooling, test output, and code review.

### 3.2 Loss of control

Agent activity can be opaque. Developers may not know:

* what is currently being changed
* which commands have run
* what assumptions the agent made
* which files belong to which task
* whether human edits will be preserved
* how much the task has cost
* whether a branch is safe to merge

### 3.3 Unsafe parallelism

Running multiple coding agents against one checkout risks:

* overlapping edits
* uncommitted changes being overwritten
* unclear task ownership
* test interference
* difficult rollback
* merge conflicts discovered too late

### 3.4 Weak intervention workflows

Most agent interfaces treat human involvement primarily as:

* approving a tool call
* replying in chat
* accepting or rejecting a generated patch

They do not treat direct manual coding as a first-class part of the agent lifecycle.

---

## 4. Opportunity

Forge can occupy the space between terminal coding agents and traditional editors.

Traditional editors are designed around humans writing code with AI assistance.

Coding-agent interfaces are designed around humans giving tasks to autonomous workers.

Forge should be designed around a third model:

> Humans and agents alternately take control of the same development task while sharing repository, task, and execution context.

The central unit of work is not a chat conversation. It is a repository task with:

* an objective
* an assigned agent
* a branch and optional worktree
* relevant context
* files changed
* commands executed
* test and diagnostic results
* human interventions
* final integration state

---

## 5. Target Users

### 5.1 Primary user

A terminal-first software engineer who already uses coding agents for repository work and wants better control over substantial or parallel tasks.

Typical characteristics:

* comfortable with Git and command-line tools
* uses Claude Code, Codex, OpenCode, or similar products
* regularly inspects or edits agent-generated code
* works on repositories large enough that context matters
* values model choice and local control
* distrusts opaque autonomous execution
* may already use branches or worktrees manually

### 5.2 Initial user segments

#### Senior engineers

Need to delegate implementation while retaining architectural judgment.

#### Open-source maintainers

Need to handle several issues or pull requests without mixing working state.

#### Infrastructure and platform engineers

Need visibility into commands, configuration changes, tests, permissions, and side effects.

#### AI-tool power users

Use multiple models and want a neutral workspace rather than a single-vendor environment.

#### Small engineering teams

Need repeatable agent workflows without immediately adopting an enterprise platform.

### 5.3 Secondary users

* engineering managers reviewing agent-assisted work
* staff engineers coordinating cross-cutting changes
* teams using local models or custom gateways
* security-conscious organisations requiring inspectable local execution

### 5.4 Users not initially targeted

* developers seeking a complete graphical IDE
* beginners unfamiliar with Git or terminals
* teams that do not yet use coding agents
* enterprises requiring SSO, central governance, or remote fleet administration
* users seeking fully autonomous software delivery

---

## 6. Jobs to Be Done

### Core job

> When I delegate repository work to an AI agent, help me supervise, correct, and integrate its work without losing context or control.

### Supporting jobs

1. When an agent is working, show me what it is doing and what it is waiting for.

2. When I need to intervene, let me inspect and edit the relevant code without leaving Forge.

3. When I change code manually, ensure the agent understands and preserves my changes.

4. When several tasks can run independently, isolate them so that agents do not corrupt each other’s work.

5. When an agent finishes, show me the diff, tests, diagnostics, assumptions, and unresolved risks.

6. When changes conflict, help me understand and resolve the conflict before integration.

7. When choosing a model, let me balance capability, speed, privacy, and cost.

8. When a session is interrupted, preserve enough state for work to continue safely.

---

## 7. Design Principles

### 7.1 Human authority

The developer remains the final authority.

Every task must support:

* pause
* inspect
* intervene
* redirect
* edit
* approve
* reject
* discard

### 7.2 Transparent execution

Forge should make important activity visible:

* active agent
* task state
* model
* tools
* commands
* permissions
* branch
* worktree
* changed files
* tests
* cost
* errors
* pending decisions

No invisible agent magic.

### 7.3 Isolation before autonomy

Parallel writing tasks should be isolated before Forge increases agent autonomy.

The default relationship is:

```text
Writing task
└── Agent
    └── Git branch
        └── Git worktree
```

Read-only exploration agents may share repository access when safe.

### 7.4 Manual intervention is normal

Human correction is not an agent failure state. It is a supported development mode.

Forge should make the transition between agent control and human control inexpensive.

### 7.5 Progressive complexity

Forge should not require subagents, worktrees, or advanced orchestration for simple tasks.

A single prompt in the current checkout must remain easy.

### 7.6 Model independence

Agent workflows, tools, task state, and Git state must not depend on one model provider.

### 7.7 Open interfaces

Prefer open protocols and inspectable formats, including:

* MCP for tool integration
* Git for source-control state
* standard repository instruction files
* portable skill definitions
* readable session and task artefacts

### 7.8 Decaying scaffolding

Forge should avoid rigid orchestration that becomes unnecessary as models improve.

Agent roles, planning stages, review loops, and context management should be configurable and removable.

### 7.9 Trust over apparent autonomy

A smaller task completed reliably is more valuable than a larger task completed opaquely.

---

## 8. Canonical Product Workflow

```text
Developer opens a repository
→ creates or describes a task
→ Forge gathers repository context
→ Forge proposes a plan when needed
→ developer approves or modifies the plan
→ Forge executes the task
→ developer observes files, commands, and progress
→ Forge requests intervention when blocked
→ relevant source or diff opens in the workspace
→ developer edits code or provides direction
→ Forge incorporates the human changes
→ Forge resumes implementation
→ tests and diagnostics run
→ developer reviews the final task diff
→ developer merges, revises, exports, or discards the result
```

For parallel work:

```text
Developer identifies independent tasks
→ Forge creates one task record per unit of work
→ each writing task receives a branch and worktree
→ agents operate concurrently within configured limits
→ Forge presents status and ownership centrally
→ developer intervenes in any task
→ completed tasks are reviewed independently
→ Forge detects conflicts and integration order
→ developer merges selected work
```

---

## 9. Product Scope

### 9.1 Core agent execution

Forge must:

* accept natural-language coding tasks
* inspect repository files and structure
* execute schema-validated tools
* edit files
* execute commands
* run tests, linters, and formatters
* report progress and failures
* maintain task context
* recover interrupted sessions where practical
* stop when the task is complete, blocked, or requires approval

### 9.2 Human workspace

Forge must provide a terminal-native workspace containing:

* agent conversation
* file tree
* syntax-highlighted file viewer
* lightweight text editing
* diff viewer
* command and terminal output
* test results
* diagnostics
* task and agent status
* branch and worktree status
* permission prompts
* model and context information

The exact layout may adapt to terminal size.

### 9.3 Tasks

A task represents a unit of repository work.

Each task should include:

* task ID
* title
* objective
* status
* assigned agent
* parent task, when applicable
* model and configuration
* branch
* worktree path
* relevant files
* activity history
* commands executed
* changed files
* test status
* intervention requests
* cost and token usage where available
* completion summary

Suggested states:

```text
draft
planned
queued
running
waiting_for_user
paused
blocked
review_required
completed
failed
discarded
merged
```

### 9.4 Agents and subagents

Forge should support:

#### Coordinator agent

Responsible for:

* interpreting the user objective
* proposing task decomposition
* delegating bounded work
* monitoring task status
* presenting integration decisions
* avoiding duplicate or conflicting delegation

#### Exploration subagent

Read-only by default.

Responsible for:

* repository discovery
* architecture questions
* locating relevant code
* dependency analysis
* collecting evidence for implementation

#### Implementation subagent

Write-capable.

Responsible for:

* implementing one bounded task
* operating in an assigned worktree
* running relevant validation
* returning a structured completion summary

#### Test or investigation subagent

Responsible for:

* reproducing failures
* analysing tests and logs
* identifying likely causes
* proposing or implementing constrained fixes

#### Review subagent

Read-only by default.

Responsible for:

* reviewing a completed diff
* identifying defects, regressions, missing tests, or policy violations
* producing evidence-backed findings

Agent roles should remain templates, not a mandatory fixed hierarchy.

### 9.5 Git branches and worktrees

Forge should support:

* creating a branch for a task
* creating a worktree for a writing agent
* listing task-to-worktree mappings
* showing dirty state
* showing commits and diffs
* detecting when a branch is already checked out
* removing abandoned worktrees safely
* merging or cherry-picking completed changes
* detecting integration conflicts
* preserving uncommitted user changes
* preventing accidental edits outside the assigned workspace

Default rules:

1. A writing subagent receives its own branch.
2. A parallel writing subagent receives its own worktree.
3. A read-only subagent does not require a worktree.
4. Forge must not silently discard uncommitted changes.
5. Forge must ask before destructive Git operations.
6. Integration remains user-controlled by default.

### 9.6 Human intervention

An agent may request intervention when:

* requirements are ambiguous
* two implementation approaches have meaningful trade-offs
* credentials or external access are required
* a destructive action is necessary
* tests reveal an unrelated repository problem
* architectural judgment is needed
* merge conflicts cannot be safely resolved automatically
* the agent has low confidence

Forge should:

* identify the task requesting attention
* explain the decision required
* open the relevant file, diff, output, or diagnostic
* allow the developer to edit or respond
* preserve the developer’s changes
* update agent context
* resume from the correct task state

### 9.7 Lightweight editor

The initial editor should support:

* opening files
* syntax highlighting
* cursor movement
* insert and delete
* selection
* copy and paste through supported terminal mechanisms
* search within a file
* save
* undo and redo
* line-number display
* jump to line
* opening files from diagnostics or diffs
* external-editor handoff through `$EDITOR`

The initial editor does not need to provide complete Vim compatibility.

Potential later capabilities:

* modal editing
* configurable keymaps
* symbols and references
* LSP-backed navigation
* multiple buffers
* split views
* structural selection
* refactoring support

### 9.8 Diff and review

Forge must show:

* task-scoped changes
* worktree-scoped changes
* staged and unstaged changes
* added, modified, deleted, and renamed files
* inline or side-by-side diff where terminal size permits
* agent attribution where known
* human modifications after agent changes
* test status associated with the diff
* unresolved review findings

The developer should be able to:

* accept the task result
* request revisions
* edit the changed code
* revert selected changes
* discard the task
* commit
* merge or cherry-pick

### 9.9 Tests and diagnostics

Forge should:

* display commands executed by agents
* stream command output
* show exit status
* parse common test and compiler diagnostics where practical
* associate failures with a task
* open relevant files and lines
* rerun commands after intervention
* distinguish product failures from repository failures
* retain validation results in the task summary

### 9.10 Models and providers

Forge should support:

* multiple hosted model providers
* local inference endpoints
* model selection by user
* model selection by task or agent
* provider configuration without changing agent logic
* capability-aware tool exposure where necessary
* token and cost reporting where providers expose usage
* different models for exploration, implementation, and review

Automatic model routing may be added later but is not required for the initial product.

### 9.11 Tools, MCP, skills, and hooks

Forge should support:

* built-in repository tools
* schema-validated tool arguments
* MCP tool discovery and invocation
* reusable skills
* project-level instructions
* user-level instructions
* lifecycle hooks
* custom slash commands
* permission policies by tool and agent

Skills may contribute:

* instructions
* prompts
* commands
* tool declarations
* hooks
* configuration defaults

Skills must not silently receive unrestricted execution privileges.

### 9.12 Context and session management

Forge should:

* persist conversations and task state
* track repository and worktree identity
* compact long histories
* offload large tool results
* create structured task handoffs
* preserve user decisions and interventions
* restore interrupted sessions
* distinguish current repository state from stale agent assumptions

The repository remains the source of truth for code.

---

## 10. Functional Requirements

### 10.1 Core execution

| ID      | Requirement                                                                                                 | Priority |
| ------- | ----------------------------------------------------------------------------------------------------------- | -------- |
| CORE-01 | Every tool must declare enforceable input and output contracts.                                             | P0       |
| CORE-02 | Invalid tool arguments must be rejected before side effects.                                                | P0       |
| CORE-03 | Forge must support built-in and MCP tools through a consistent invocation path.                             | P0       |
| CORE-04 | Agents must be able to inspect, edit, and validate repository code.                                         | P0       |
| CORE-05 | Model, tool, and command failures must be visible to the user.                                              | P0       |
| CORE-06 | Forge must produce a completion summary containing changes, validation, assumptions, and unresolved issues. | P0       |

### 10.2 Workspace

| ID     | Requirement                                                | Priority |
| ------ | ---------------------------------------------------------- | -------- |
| WSP-01 | Forge must provide a full-screen terminal workspace.       | P0       |
| WSP-02 | Users must be able to browse repository files.             | P0       |
| WSP-03 | Users must be able to view syntax-highlighted source.      | P0       |
| WSP-04 | Users must be able to inspect task-scoped diffs.           | P0       |
| WSP-05 | Users must be able to make and save basic manual edits.    | P1       |
| WSP-06 | Forge must support opening the current file in `$EDITOR`.  | P0       |
| WSP-07 | Forge must show command output, tests, and diagnostics.    | P0       |
| WSP-08 | Forge must preserve and recognise human file edits.        | P0       |
| WSP-09 | The agent must refresh relevant context after human edits. | P0       |

### 10.3 Tasks and agents

| ID      | Requirement                                                         | Priority |
| ------- | ------------------------------------------------------------------- | -------- |
| TASK-01 | Forge must represent delegated work as explicit tasks.              | P0       |
| TASK-02 | Each task must have an observable lifecycle state.                  | P0       |
| TASK-03 | A user must be able to pause, resume, cancel, and discard a task.   | P0       |
| TASK-04 | Forge must support at least one read-only subagent.                 | P1       |
| TASK-05 | Forge must support bounded implementation subagents.                | P1       |
| TASK-06 | Concurrent agent count must be configurable and limited.            | P1       |
| TASK-07 | The coordinator must not silently create unlimited tasks or agents. | P0       |
| TASK-08 | Users must see which agent owns each active task.                   | P1       |
| TASK-09 | Every subagent must return a structured result to its parent.       | P1       |

### 10.4 Git isolation

| ID     | Requirement                                                           | Priority |
| ------ | --------------------------------------------------------------------- | -------- |
| GIT-01 | Forge must detect the current repository and branch.                  | P0       |
| GIT-02 | Forge must detect uncommitted changes before starting isolated work.  | P0       |
| GIT-03 | Forge must create a dedicated branch for a parallel writing task.     | P1       |
| GIT-04 | Forge must create a dedicated worktree for a parallel writing task.   | P1       |
| GIT-05 | Forge must display task, branch, and worktree mappings.               | P1       |
| GIT-06 | Forge must not silently overwrite or discard user changes.            | P0       |
| GIT-07 | Destructive Git operations must require approval.                     | P0       |
| GIT-08 | Forge must detect merge conflicts before integration.                 | P1       |
| GIT-09 | The user must control final merge, cherry-pick, or discard.           | P0       |
| GIT-10 | Forge must safely clean up worktrees after completion or abandonment. | P2       |

### 10.5 Intervention

| ID      | Requirement                                                                           | Priority |
| ------- | ------------------------------------------------------------------------------------- | -------- |
| HITL-01 | Agents must be able to enter a waiting-for-user state.                                | P0       |
| HITL-02 | Forge must show the exact decision or information required.                           | P0       |
| HITL-03 | Forge must link intervention requests to relevant code, diff, output, or diagnostics. | P1       |
| HITL-04 | Users must be able to edit code while a task is paused.                               | P1       |
| HITL-05 | Resumed agents must incorporate human changes without reverting them.                 | P0       |
| HITL-06 | Approval prompts must identify the task, command, risk, and affected scope.           | P0       |

### 10.6 Review and validation

| ID     | Requirement                                                               | Priority |
| ------ | ------------------------------------------------------------------------- | -------- |
| REV-01 | Forge must show changed files and unified diffs.                          | P0       |
| REV-02 | Forge must associate validation results with the task revision tested.    | P1       |
| REV-03 | Users must be able to request revisions after review.                     | P0       |
| REV-04 | A review agent may inspect completed work with independent context.       | P2       |
| REV-05 | Forge must distinguish verified, failed, skipped, and unavailable checks. | P0       |
| REV-06 | Forge must not claim success when required validation did not run.        | P0       |

### 10.7 Context and durability

| ID     | Requirement                                                                                   | Priority |
| ------ | --------------------------------------------------------------------------------------------- | -------- |
| CTX-01 | Large tool outputs must be stored outside the active model context when appropriate.          | P0       |
| CTX-02 | Long sessions must support context compaction or structured handoff.                          | P0       |
| CTX-03 | Task state must survive normal application restart.                                           | P0       |
| CTX-04 | Forge should recover interrupted task state without repeating known destructive side effects. | P1       |
| CTX-05 | Agent context must include task ownership and workspace identity.                             | P0       |
| CTX-06 | Forge must detect when repository state changed outside the current agent turn.               | P1       |

### 10.8 Safety and permissions

| ID      | Requirement                                                              | Priority |
| ------- | ------------------------------------------------------------------------ | -------- |
| SAFE-01 | Tools must be classified by side-effect and risk level.                  | P0       |
| SAFE-02 | Users must be able to allow, deny, or conditionally approve tools.       | P0       |
| SAFE-03 | Agents must operate only within their authorised workspace scope.        | P0       |
| SAFE-04 | Secrets must not be rendered in ordinary conversation or logs.           | P0       |
| SAFE-05 | Network and external-service access must be visible and configurable.    | P1       |
| SAFE-06 | Each subagent must inherit no more privilege than required for its task. | P1       |

### 10.9 Model portability

| ID     | Requirement                                                                       | Priority |
| ------ | --------------------------------------------------------------------------------- | -------- |
| MDL-01 | Agent logic must not be tied to a single provider.                                | P0       |
| MDL-02 | Users must be able to select provider and model through configuration or the TUI. | P0       |
| MDL-03 | Forge must expose model identity and context usage in the workspace.              | P0       |
| MDL-04 | Forge should display token and cost information when available.                   | P1       |
| MDL-05 | Different tasks or agent roles may use different models.                          | P2       |

---

## 11. User Experience Requirements

### 11.1 Main workspace regions

The interface should support these conceptual regions:

```text
┌─────────────────────────────────────────────────────────────┐
│ Repository · Branch · Task · Model · Context · Cost         │
├──────────────┬────────────────────────────┬─────────────────┤
│ Files        │ Conversation / Editor      │ Tasks / Agents  │
│              │                            │                 │
│              │                            │                 │
├──────────────┴────────────────────────────┴─────────────────┤
│ Diff · Tests · Diagnostics · Terminal · Activity            │
├─────────────────────────────────────────────────────────────┤
│ Input / command palette / contextual actions                │
└─────────────────────────────────────────────────────────────┘
```

This is a conceptual structure, not a required fixed layout.

On narrow terminals, Forge may:

* hide side panels
* use tabs
* use overlays
* prioritise the active task and input
* preserve essential task and model identity

### 11.2 Keyboard-first operation

All critical flows must be usable without a mouse:

* navigating files
* changing panels
* opening tasks
* approving actions
* reviewing diffs
* entering commands
* editing text
* closing overlays
* switching between agent and human control

### 11.3 Status visibility

Forge must visibly distinguish:

* running
* waiting
* blocked
* paused
* failed
* review required
* completed
* merged

A task must never appear idle when it is actually blocked on user input or an error.

### 11.4 Progressive disclosure

Simple tasks should initially show:

* conversation
* current activity
* changed files
* final diff

Worktree, branch, subagent, and advanced diagnostic detail should appear as needed.

---

## 12. Non-Goals

### Initial non-goals

| Non-goal                                 | Rationale                                                                    |
| ---------------------------------------- | ---------------------------------------------------------------------------- |
| Complete Vim or Neovim compatibility     | Manual intervention matters; recreating decades of editor behaviour does not |
| Graphical desktop IDE                    | Forge is initially terminal-native                                           |
| Unlimited autonomous agent swarms        | Difficult to control, expensive, and weakly tied to user value               |
| Automatic merging without review         | Final integration remains user-controlled                                    |
| Replacing Git                            | Git is the source-control foundation                                         |
| Replacing model providers                | Forge orchestrates external or local models                                  |
| Replacing CI/CD systems                  | Forge may invoke and display checks but is not a CI platform                 |
| Enterprise identity and fleet management | Defer until individual and small-team workflow is validated                  |
| Hosted remote execution platform         | Local repository work comes first                                            |
| Proprietary plugin protocol              | Prefer MCP and portable skills                                               |
| Knowledge graph as a core dependency     | Repository retrieval should not require Graphify or an equivalent system     |
| Full debugger implementation             | Diagnostics and command output come before debugger replacement              |
| Autonomous product management            | Forge may assist with planning but does not decide product intent            |

---

## 13. Product Phases

### Phase 1 — Trustworthy single-agent workflow

Deliver:

* reliable repository inspection
* file editing
* command execution
* tests and diagnostics
* session persistence
* permission controls
* clear diff review
* model/provider configuration
* completion summaries
* polished terminal interaction

Exit criteria:

* Forge can complete medium-sized repository tasks reliably.
* Failures are visible and recoverable.
* Developers can review exactly what changed.
* Forge never silently claims unverified success.

### Phase 2 — Human intervention workspace

Deliver:

* file tree
* source viewer
* lightweight editing
* `$EDITOR` integration
* jump from errors to files
* pause and resume
* preservation of human edits
* agent context refresh after manual changes

Exit criteria:

* A developer can correct an agent’s work and resume the task without re-explaining repository context.
* Time spent leaving Forge during a task decreases materially.

### Phase 3 — Constrained subagents

Deliver:

* explicit task model
* coordinator delegation
* read-only exploration agents
* bounded implementation agents
* configurable concurrency
* task and agent status
* structured subagent results

Exit criteria:

* Delegated tasks have clear ownership.
* Agent results can be reviewed independently.
* Parallelism does not reduce task reliability unacceptably.

### Phase 4 — Worktree-isolated parallel development

Deliver:

* branch creation
* worktree lifecycle
* task-to-worktree mapping
* per-task diffs
* conflict detection
* integration controls
* cleanup flows

Exit criteria:

* Multiple writing agents can work simultaneously without sharing mutable checkout state.
* Users can safely merge, revise, or discard each task.

### Phase 5 — Deeper agent-native IDE capabilities

Potential scope:

* LSP integration
* symbol navigation
* richer editing
* configurable keymaps
* multiple buffers
* conflict-resolution interface
* agent attribution in diffs
* model routing
* task templates
* reusable team workflows

This phase should be driven by observed intervention patterns.

### Phase 6 — Team and commercial capabilities

Potential scope:

* shared skills and instructions
* policy configuration
* centrally managed model access
* usage and cost reporting
* audit export
* remote runners
* shared task templates
* organisation controls
* enterprise deployment

This phase is not required to validate the core product.

---

## 14. Success Metrics

### 14.1 North-star metric

**Trusted tasks completed in Forge**

A trusted task is one that:

* produced repository changes
* exposed its final diff
* reported validation honestly
* was accepted, committed, or merged by the user

### 14.2 Product metrics

| Metric                                                             | Why it matters                       |
| ------------------------------------------------------------------ | ------------------------------------ |
| Task completion rate                                               | Measures basic usefulness            |
| Accepted task rate                                                 | Indicates trust in output            |
| Median time to accepted change                                     | Measures workflow efficiency         |
| User interventions per task                                        | Shows where autonomy is insufficient |
| Successful resume after intervention                               | Tests the core human–agent thesis    |
| Time spent outside Forge                                           | Measures context-switch reduction    |
| Revision rate after agent completion                               | Indicates output quality             |
| Percentage of parallel tasks completed without workspace collision | Validates worktree isolation         |
| Test pass rate at review                                           | Measures implementation reliability  |
| Tasks abandoned because of tool or product failure                 | Identifies core friction             |
| Cost per accepted task                                             | Measures economic efficiency         |
| Weekly retained active developers                                  | Measures habit formation             |

### 14.3 Initial qualitative validation

Forge should be considered directionally validated when experienced coding-agent users report that:

* they prefer Forge for at least one recurring repository workflow
* intervention is easier than returning to a separate editor and agent session
* worktree automation reduces parallel-task friction
* agent activity feels understandable rather than opaque
* they trust Forge not to destroy or mix repository state

---

## 15. Acceptance Scenarios

### Scenario A — Single-agent implementation

1. User opens a repository.
2. User asks Forge to add a bounded feature.
3. Forge inspects relevant code.
4. Forge modifies files.
5. Forge runs formatter and tests.
6. Forge displays changed files and validation.
7. User reviews and accepts the result.

**Pass condition:** The task can be completed without leaving Forge, and unverified checks are not reported as successful.

### Scenario B — Manual correction

1. Agent implements a feature incorrectly.
2. User pauses the task.
3. User opens the affected file.
4. User edits the implementation.
5. User resumes the task.
6. Agent preserves the edit and completes tests.

**Pass condition:** The user does not need to explain the edit again, and the agent does not revert it unintentionally.

### Scenario C — Agent requests clarification

1. Agent identifies two materially different approaches.
2. Agent enters `waiting_for_user`.
3. Forge presents the decision and affected files.
4. User selects an approach or edits the relevant code.
5. Agent resumes.

**Pass condition:** The task state remains intact and the decision is included in subsequent context.

### Scenario D — Parallel implementation

1. User creates two independent tasks.
2. Forge creates separate branches and worktrees.
3. Two agents work concurrently.
4. Each agent runs its own validation.
5. Forge shows separate diffs.
6. User integrates both tasks.

**Pass condition:** Neither task modifies the other task’s working directory, and integration conflicts are reported before merge.

### Scenario E — Failed parallel task

1. One agent completes.
2. Another agent fails or produces unacceptable changes.
3. User merges the successful task.
4. User discards the failed task and removes its worktree.

**Pass condition:** Discarding one task does not affect the accepted task or primary checkout.

### Scenario F — External editor handoff

1. User opens a file from Forge in `$EDITOR`.
2. User saves changes and exits.
3. Forge detects the file change.
4. Forge refreshes the diff and agent context.
5. Agent continues.

**Pass condition:** The handoff does not lose task state or overwrite the external edit.

---

## 16. Risks and Mitigations

### 16.1 Scope explosion

**Risk:** Forge attempts to become a complete editor, agent platform, Git client, orchestration system, and enterprise runtime simultaneously.

**Mitigation:**

* prioritise the canonical workflow
* keep editing lightweight initially
* use Git rather than abstracting it away
* delay enterprise capabilities
* require evidence before deepening editor functionality

### 16.2 Weak differentiation

**Risk:** Forge becomes another terminal coding agent with panels for tasks and diffs.

**Mitigation:**

* optimise explicitly for intervention and resumption
* make worktree isolation effortless
* expose agent ownership and state
* demonstrate human takeover as a signature workflow
* measure reduction in external context switching

### 16.3 Subagent cost without value

**Risk:** Multiple agents consume more tokens but do not improve completion speed or quality.

**Mitigation:**

* constrain concurrency
* require explicit task boundaries
* prefer read-only subagents initially
* show cost by task
* compare parallel and sequential outcomes
* allow users to disable orchestration

### 16.4 Merge complexity

**Risk:** Parallel agents create conflicts that erase productivity gains.

**Mitigation:**

* isolate tasks by worktree
* analyse likely file overlap before delegation
* make ownership visible
* warn about conflicting scope
* allow sequential integration
* keep the final merge under human control

### 16.5 Editor quality expectations

**Risk:** Terminal users compare Forge’s editor with Vim or Neovim.

**Mitigation:**

* describe editing as an intervention surface
* support `$EDITOR`
* avoid promising full Vim compatibility
* deepen editing only around observed workflows

### 16.6 Stale agent context

**Risk:** Human or external changes invalidate agent assumptions.

**Mitigation:**

* track workspace revisions
* detect changed files
* refresh relevant context
* require revalidation after modifications
* clearly distinguish tested and untested revisions

### 16.7 Loss of user trust

**Risk:** Forge overwrites changes, hides errors, or reports success inaccurately.

**Mitigation:**

* never silently discard dirty state
* preserve task-scoped audit history
* show commands and exit status
* distinguish skipped checks
* require approval for destructive operations
* prioritise trust over apparent autonomy

---

## 17. Assumptions

1. Coding agents will continue to improve but will still require human judgment for meaningful repository work.

2. Git branches and worktrees are acceptable isolation primitives for the initial local product.

3. Developers will tolerate basic built-in editing when deeper editing remains available through `$EDITOR`.

4. Model providers remain external or locally hosted; Forge does not train foundation models.

5. MCP remains useful for interoperable external tools.

6. Repository files and Git state remain the source of truth for code.

7. Early users are comfortable with terminal workflows.

8. Local-first execution is sufficient for initial product validation.

9. Parallel agents are useful only when work can be decomposed into bounded tasks.

10. Enterprise governance should follow, not precede, strong individual-developer adoption.

---

## 18. Open Product Questions

1. Should task creation always be explicit, or may Forge create temporary internal tasks automatically?

2. Should implementation subagents commit changes automatically or leave them uncommitted for review?

3. What is the default maximum number of concurrent agents?

4. When should Forge recommend parallel execution rather than sequential work?

5. How should Forge estimate likely file overlap before creating worktrees?

6. Should the built-in editor use conventional bindings, optional Vim-style bindings, or both?

7. Which human edits should trigger automatic agent context refresh?

8. How should Forge distinguish agent-authored and human-authored lines after subsequent edits?

9. Should review agents run automatically or only when requested?

10. What session and task artefacts should be committed to the repository versus stored in Forge’s local state?

11. How should Forge manage shared dependencies such as build caches, databases, and ports across worktrees?

12. Which capabilities belong in the open-source product versus a future paid team offering?

---

## 19. Immediate Product Decisions

The following decisions are recommended for the first implementation cycle:

1. Forge remains a terminal-first application.

2. Forge evolves from a conversation TUI into an agent-native development workspace.

3. The initial built-in editor supports intervention, not full Vim replacement.

4. `$EDITOR` handoff remains a first-class workflow.

5. Tasks become explicit persisted entities.

6. Read-only subagents are implemented before parallel writing agents.

7. Parallel writing agents use separate branches and Git worktrees.

8. The user controls final integration.

9. Agent and task activity must remain visible.

10. Core agent reliability takes priority over broad orchestration.

11. Graphify and repository knowledge graphs remain optional future integrations.

12. Enterprise governance remains outside the initial product boundary.

---

## 20. Recommended Near-Term Allocation

Until the core workflow is validated:

* **60% — Core agent reliability**

  * repository exploration
  * editing
  * tool execution
  * validation
  * context handling
  * failure recovery

* **25% — Human intervention workspace**

  * source viewing
  * diffs
  * basic editing
  * diagnostics
  * pause/resume
  * external-editor handoff

* **15% — Subagent and worktree experiments**

  * task model
  * read-only delegation
  * isolated implementation prototype
  * task status UI

Forge should not commit heavily to deeper IDE functionality or large-scale multi-agent orchestration until real usage demonstrates repeated demand.

---

## 21. Final Product Definition

> Forge is an open, terminal-native development workspace for running, steering, and reviewing coding agents.

It gives developers one place to delegate repository work, inspect agent activity, intervene directly in code, coordinate isolated parallel tasks, validate results, and safely integrate accepted changes.

Forge succeeds when developers can delegate more work without surrendering understanding, judgment, or control.
