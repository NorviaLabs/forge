# Forge 2026 TUI implementation manifest

Baseline `dfa46a07aa43551395ba7cb6b4867ab2d8a9c324`. Complete in dependency order. Every item links to `07-implementation-plan.md`; consult the named design/agent/component section and prototype state before editing.

- [ ] **DESIGN-001 — Semantic palette and constants.** Spec: `03` semantic tokens/cell metrics; prototype 01–30; plan Phase 0. Modules: `forge-config/src/theme.rs`, built-in theme TOML, `forge-tui/src/theme.rs`.
- [ ] **DESIGN-002 — State markers and text roles.** Spec: `03` typography/glyphs, `04-D/E`; prototype 06–15; plan Phase 0. Modules: `forge-tui/src/status_glyph.rs`, plan/tool renderers.
- [ ] **DESIGN-003 — Deterministic responsive layout.** Spec: `03` responsive geometry, `05` Application frame; prototype 28–30; plan Phase 1. Modules: `layout.rs`, `app/render.rs`, `widgets/bottom_panel.rs`.
- [ ] **DESIGN-004 — Pane chrome and focus.** Spec: `03` border/focus rules, `05` pane components; prototype 01, 19–23; plan Phase 1. Modules: `app/render.rs`, `widgets/panel.rs`, Files/editor/chat/terminal renderers.
- [ ] **DESIGN-005 — Stable per-turn projection.** Spec: `04-B/H`, `05` Agent turn; prototype 15–18; plan Phase 2. Modules: `forge-transcript/src/lib.rs`, `app/turn.rs`, `app/types.rs`, `app/render.rs`, `conversation.rs`.
- [ ] **DESIGN-006 — Live row and streaming answer.** Spec: `04-B/C`; prototype 04–09, 14; plan Phase 2. Modules: `widgets/turn_line.rs`, `app/render.rs`, `conversation.rs`.
- [ ] **DESIGN-007 — Historical compaction and anchoring.** Spec: `04-H`; prototype 16–18; plan Phase 2. Modules: transcript projection, `conversation.rs`, render/cache/scroll state.
- [ ] **DESIGN-008 — Truthful tool rows.** Spec: `04-D`, `05` Tool row/Error; prototype 07, 11–13; plan Phase 3. Modules: `forge-transcript/src/lib.rs`, tool formatters, `conversation.rs`.
- [ ] **DESIGN-009 — Shell sessions and tool groups.** Spec: `04-D` grouping; prototype 08–09; plan Phase 3. Modules: transcript projection, tool parsers, conversation renderer.
- [ ] **DESIGN-010 — Plan hierarchy.** Spec: `04-E`, `05` Plan; prototype 06, 15; plan Phase 3. Modules: `forge-transcript/src/lib.rs`, plan renderer, `app/render.rs`.
- [ ] **DESIGN-011 — Final response and completion.** Spec: `04-H`, `05` Chat/Agent turn; prototype 14–17; plan Phase 3. Modules: `conversation.rs`, Markdown helpers, `app/turn.rs`.
- [ ] **DESIGN-012 — Composer/footer/status.** Spec: `03` cell metrics, `05` Composer/Footer/Model status; prototype 01–05, 19–22; plan Phase 4. Modules: `widgets/input.rs`, `widgets/footer.rs`, `app/render.rs`.
- [ ] **DESIGN-013 — Approvals and questions.** Spec: `04-G`, `05` Approval; prototype 10; plan Phase 4. Modules: `app/approvals.rs`, `app/overlays.rs`, question/approval widgets.
- [ ] **DESIGN-014 — Common overlay family.** Spec: `03` modal geometry, `05` overlay components; prototype 25, 27; plan Phase 5. Modules: `overlays.rs`, `app/overlays.rs`, shared modal widgets.
- [ ] **DESIGN-015 — Model picker layout.** Spec: `05` Model selector; prototype 24; plan Phase 5. Modules: `overlays.rs`, model picker/filter state.
- [ ] **DESIGN-016 — Files tree and search.** Spec: `05` Files/Search/Tree; prototype 23; plan Phase 6. Modules: `file_explorer.rs`, workspace navigation.
- [ ] **DESIGN-017 — Editor, terminal, diff, conflicts.** Spec: `05` workspace components; prototype 19–22, 26–27; plan Phase 6. Modules: `editor.rs`, `editor_session.rs`, `interactive_terminal.rs`, `bottom_panel.rs`, `diff_view.rs`, `app/diff.rs`, `app/workspace.rs`.
- [ ] **DESIGN-018 — Consistency and release gate.** Spec: all plus `09-validation-plan.md`; prototype 01–30; plan Phase 7. Modules: TUI visual/render/performance tests and, after acceptance, `FORGE-DESIGN.md`.

For every checkbox: preserve current bindings/data; add focused behavior tests; capture real-PTY before/after evidence; verify dark and light themes; record any unavailable metadata as omitted/unknown rather than inferred. A checkbox is incomplete if expanded detail loses information or if its visual acceptance criteria have not been exercised at the specified widths.
