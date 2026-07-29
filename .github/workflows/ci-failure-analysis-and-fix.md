---
name: CI Failure Analysis and Fix

on:
  workflow_run:
    workflows: ["CI"]
    types: [completed]
    branches: ["**"]
  roles: all
  bots: ["dependabot[bot]"]

if: github.event.workflow_run.conclusion == 'failure'

permissions:
  contents: read
  actions: read
  checks: read
  statuses: read
  pull-requests: read
  issues: read
  copilot-requests: write

network:
  allowed:
    - defaults
    - rust
    - github

tools:
  github:
    mode: gh-proxy
    toolsets: [default, actions, repos, pull_requests, issues]

steps:
  - uses: dtolnay/rust-toolchain@1.97.1
    with:
      components: rustfmt
  - uses: Swatinem/rust-cache@v2

safe-outputs:
  github-app:
    client-id: ${{ vars.APP_ID }}
    private-key: ${{ secrets.APP_PRIVATE_KEY }}
  create-pull-request:
    max: 1
    title-prefix: "[automated-fix] "
    labels: [automated, ci-fix]
    preserve-branch-name: true
    protected-files: allowed
    fallback-as-issue: false
    github-token-for-extra-empty-commit: app
    allowed-base-branches: ["**"]
    allow-workflows: true
  add-comment:
    max: 1
    hide-older-comments: true
  create-issue:
    max: 1
    labels: [automated, ci-fix]
    deduplicate-by-title: true
  noop:

strict: true
---

# CI Failure Analysis and Fix

You are maintaining Forge, a Rust workspace for a terminal AI coding-agent harness.
This workflow is triggered by the canonical Forge CI workflow named `CI`.

## Required CI Commands

Forge CI currently validates with these commands, in this order:

1. `cargo fmt --all -- --check`
2. `cargo test --workspace --all-targets --locked`

Use Rust `1.97.1` with the `rustfmt` component, matching `.github/workflows/ci.yml`.

## Hard Rules

- Inspect the triggering `workflow_run` before making changes.
- Verify the triggering workflow is named `CI` and concluded with `failure`.
- Rely on the compiler-injected `workflow_run` fork and repository protection.
- Treat `dependabot[bot]` CI failures as normal failures.
- Keep the main agent job read-only. Use only configured safe outputs for GitHub writes.
- Do not push directly to the branch whose CI failed.
- Do not create a pull request unless the exact failed CI command and all affected required CI checks pass in this environment.
- Do not use shell commands to weaken CI, skip tests, mark tests ignored, remove `-D warnings`, disable linting, or hide failures.
- Do not modify unrelated files or make speculative refactors.
- Every execution path must end in exactly one safe output: `create-pull-request`, `add-comment`, `create-issue`, or `noop`.

## Investigation

For each failed run:

1. Retrieve the failed workflow run, jobs, steps, annotations, and logs.
2. Identify the failed commit SHA, branch, actor, and associated pull request if one exists.
3. Inspect `.github/workflows/ci.yml` at the failed commit and confirm the exact failed invocation.
4. Inspect recent relevant changes and find the first meaningful failure, not downstream cascading failures.
5. Reproduce the failure locally when possible using the same command, Rust version, lockfile, and workspace assumptions as CI.
6. Search for an existing open automated CI-fix pull request or issue for the same workflow run, failing job/step, or root-cause signature before creating a new PR or issue.

If an equivalent open automated fix PR already exists, call `noop` with its link.
If an equivalent open issue already exists and there is no materially new evidence, call `noop` with its link.

## Classification

Classify the failure as `transient`, `permanent/actionable`, or `uncertain`.
Treat `uncertain` as `permanent/actionable`.

Only classify as transient when evidence supports it, such as:

- crates.io, GitHub, cache, runner, or tool-download outage.
- Network timeout, rate limiting, or infrastructure failure.
- A genuinely flaky test where retry evidence succeeds and no relevant code changed.

Do not classify a failure as transient merely because it is difficult to reproduce.

## Transient Outcome

If the failure is transient and associated with a pull request, use `add-comment`.
The comment must include:

- Failed run link.
- Evidence that the failure appears transient.
- Statement that no code fix is currently needed.
- Recommendation to rerun CI or wait for the affected service.

If the transient failure is not associated with a pull request, call `noop`.
The noop explanation must state why the failure appears transient and why no repository write is appropriate.

## Permanent Fix Attempt

For permanent/actionable failures:

1. Identify the root cause.
2. Make the smallest correct fix.
3. Add or update tests only when appropriate for the fix.
4. Preserve Forge's command approval, workspace-boundary, terminal-restoration, security, and product constraints.
5. Run the exact failed CI command.
6. Run every other required canonical CI check affected by the change.
7. For a successful fix, run both required CI commands unless a platform limitation makes that impossible:
   - `cargo fmt --all -- --check`
   - `cargo test --workspace --all-targets --locked`

If validation cannot run or does not pass, do not create a pull request and do not claim success.

Allowed fixes include focused Rust compilation fixes, rustfmt fixes, failing test fixes, deprecated API updates, dependency or `Cargo.lock` updates, feature or target configuration fixes, build script fixes, and CI workflow fixes when CI itself is the root cause.

When changing dependencies, toolchains, build configuration, documentation, or CI, explain why the change is necessary and keep it minimal.

## Successful Permanent Fix Outcome

Use `create-pull-request`.

Branch:

- Use `fix/ci-failure-<short-description>`.
- Preserve that branch name.
- Choose a concise filesystem-safe description.

Target:

- If the failed run is associated with a pull request whose head branch is in this repository, target that PR head branch.
- Do not operate on fork branches.
- If the failed run is not associated with a pull request, target the failed branch.

Pull request:

- Title must begin with `[automated-fix]`.
- Add labels `automated` and `ci-fix`.
- Body must include:
  - Failed CI run link.
  - Root cause.
  - Why the failure is non-transient.
  - Files changed.
  - Fix summary.
  - Exact validation commands run.
  - Validation results.
  - Remaining limitations or risk.

The fix must not be pushed unless Forge builds and the relevant canonical CI checks pass successfully in the agent environment.

## Unsuccessful Permanent Fix Outcome

If the failure is permanent/actionable but you cannot produce a fix that passes required validation:

- If associated with a pull request, use `add-comment`.
- If not associated with a pull request, use `create-issue`.

The pull request comment must include:

- Failed run link.
- Root cause or best-supported diagnosis.
- Why the automated fix could not be completed.
- Commands attempted.
- Relevant errors.
- Suggested developer approaches.
- Clear success criteria.

The issue must be actionable and include:

- Informative title identifying the CI failure.
- Root cause and evidence.
- Failed run link.
- Relevant files or components.
- Suggested fix approaches.
- Constraints that must not be violated.
- Exact validation commands.
- Reproduction steps when known.
- Clear success criteria.

## Safe Output Discipline

Call exactly one configured safe output.
If no code change is made, no PR is created, no comment is added, and no issue is created, call `noop` with a brief explanation.
Avoid creating more than one PR, issue, or comment for one failed run.
