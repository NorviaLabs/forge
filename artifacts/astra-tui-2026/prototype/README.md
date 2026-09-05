# Forge 2026 terminal prototype

Open `index.html` directly in a browser. No server, package install, build or external assets are needed. The toolbar is a **design-study control surface**, not proposed Forge UI. No backend is connected; content represents the observed disposable formatter task and deliberately simulated states.

Every terminal row has a fixed cell count. The grid paints characters and semantic styles, with no gradients, shadows, rounded controls inside the terminal, variable-sized headings or hover-only actions. Menlo/Consolas/monospace and 12px/18px are preview choices; Forge does not control the terminal font. Physical browser pixels are not Ratatui cells. Horizontally scroll the outer browser container if the chosen terminal exceeds the browser viewport; this is not Forge pane overflow.

## Controls

- Scenario selector or Previous/Next: all 30 requested states.
- Terminal selector: 80×18, 80×24, 100×30, 120×35, 160×45, 220×55.
- Palette: dark/light semantic mapping.
- Toggle details or Ctrl+O with terminal focused: reversible illustrative detail.
- Tab/Shift+Tab: visible simulated focus owners only. F4: model picker. F1: help.
- Editor Normal state: `i` selects Insert; Esc returns to Normal.
- Model/theme scenarios: arrows move preview selection; Enter/Esc leave the illustrative overlay. This does not change a real model/provider or write a theme preference.
- Files search scenario: printable characters and Backspace change query; `zzz` illustrates the no-result state.
- Arrow/Page keys scroll sample transcript. These are study navigation when the generic terminal mock is focused, not a complete reimplementation of Forge's event router.

This prototype is a collection of precisely rendered visual states, not a functional Forge clone. It does not emulate editor buffers, shell execution, approvals, queueing, token timing, clipboard, task switching or provider authentication. The Markdown specs govern those unchanged behaviors. Modal examples show representative rows, not the complete live 112-model account catalog.

## States

| ID | State | Main spec |
|---|---|---|
| [01](index.html?state=1) | Fresh start | 05 workspace |
| [02](index.html?state=2) | Idle populated workspace | 03 responsive |
| [03](index.html?state=3) | Simple conversation | 04 B/H |
| [04](index.html?state=4) | Prompt just submitted | 04 C |
| [05](index.html?state=5) | Thinking | 04 C/F |
| [06](index.html?state=6) | Plan active | 04 E |
| [07](index.html?state=7) | Tool running | 04 D |
| [08](index.html?state=8) | Multiple tools grouped | 04 D |
| [09](index.html?state=9) | Long investigation | 04 A/C |
| [10](index.html?state=10) | Approval | 04 G |
| [11](index.html?state=11) | Error | 04 D |
| [12](index.html?state=12) | Recovery | 04 D |
| [13](index.html?state=13) | Cancellation | 04 D |
| [14](index.html?state=14) | Final streaming | 04 C/H |
| [15](index.html?state=15) | Completed turn | 04 H |
| [16](index.html?state=16) | Historical collapsed | 04 H |
| [17](index.html?state=17) | Historical expanded | 04 H |
| [18](index.html?state=18) | Long conversation | 04 H |
| [19](index.html?state=19) | Editor Normal | 05 editor |
| [20](index.html?state=20) | Editor Insert | 05 editor |
| [21](index.html?state=21) | Editor dirty | 05 editor |
| [22](index.html?state=22) | Terminal focused | 05 terminal |
| [23](index.html?state=23) | Files search | 05 Files |
| [24](index.html?state=24) | Model picker | 05 selectors |
| [25](index.html?state=25) | Theme picker | 05 selectors |
| [26](index.html?state=26) | Review Changes | 05 workspace |
| [27](index.html?state=27) | Save/Discard/Cancel | 05 dialogs |
| [28](index.html?state=28) | Narrow layout | 03 responsive |
| [29](index.html?state=29) | Standard layout | 03 responsive |
| [30](index.html?state=30) | Wide layout | 03 responsive |

## Validation evidence

`qa-results.json`: browser JavaScript errors and exact row/column geometry across 360 combinations (30 states × 6 sizes × 2 themes). This is geometry validation, not proof that every long field is simultaneously visible. All long transcripts remain scrollable; approval actions must remain inspectable. Selected screenshots in `screens/` were visually reviewed; they are illustrative proposed views, never current Forge screenshots.

`glyph-validation.png` and `glyph-metrics.json`: Chromium rendered installed Menlo, Monaco and Courier. Required ASCII state token width equals three ordinary cells in all three. Optional `⏎` falls back to a slightly different advance in Monaco/Courier, so **use the ASCII `Enter` hint as the reference implementation**. ASCII states avoid that issue. Box-drawing has an ASCII fallback. Consolas and JetBrains Mono were not installed and are later terminal validation targets, not claimed tested fonts.

The attempted terminal-browser preview could not launch because the active terminal lacked its required image protocol. Chromium/Playwright was used only for this HTML design artifact. Actual Forge investigation used the release binary through real tmux PTYs.
