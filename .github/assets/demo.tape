# Source for .github/assets/demo.gif.
#
# The GIF used to be an orphan binary with no way to regenerate it, so it drifted:
# it still showed 0.1.0-beta.4 and a footer glyph that no longer exists. Keep this
# tape in step with the UI and re-record instead of hand-editing the asset.
#
# Regenerate:
#   cargo build --release
#   sh .github/assets/demo-fixture.sh
#   vhs .github/assets/demo.tape
#   ffmpeg -t 50 -i demo.gif -vf "fps=10,split[s0][s1];[s0]palettegen=max_colors=96[p];\
#     [s1][p]paletteuse=dither=bayer:bayer_scale=3" -loop 0 -y demo.gif
#
# The ffmpeg pass is not optional. It trims the tail and quantises the palette
# (1.3M -> 1.0M). Trimming is deliberate: the tape cannot time the end of a live
# model turn, so it records past the payoff and the cut lands on the diff card
# instead of on whatever the agent did next.
#
# Notes for whoever re-records this:
#   - `Ctrl+\`` is not expressible in a tape, so the terminal is opened with the
#     /terminal command instead. That is a real command, not a workaround.
#   - The explorer needs >=116 columns. At FontSize 15 that means Width 1400;
#     1200 gives 115 and forge (correctly) refuses to draw the Files pane.
#   - The last beat is a live model call, so its timing varies. The prompt says
#     "no need to run anything" on purpose: left to itself the agent verifies
#     with `python`, which does not exist on macOS (only python3), and the take
#     ends on a wall of "command not found" that says nothing about forge.

Output .github/assets/demo.gif

Set Shell "bash"
Set FontSize 15
Set Width 1400
Set Height 820
Set Padding 12
Set Framerate 10
Set TypingSpeed 45ms

# Launch and the trust prompt are plumbing, not the story.
Hide
Type "cd /tmp/forge-demo-ws && export PYTHONDONTWRITEBYTECODE=1 && clear"
Enter
Sleep 500ms
Type "$OLDPWD/target/release/forge"
Enter
Sleep 5s
Enter
Sleep 3s
Show

# --- 1. the bug turns up in the terminal --------------------------------
Type "/terminal"
Sleep 400ms
Enter
Sleep 1.5s
Type "python3 report.py"
Sleep 300ms
Enter
Sleep 3s

# --- 2. fix it by hand, in the same window ------------------------------
Escape
Sleep 600ms
# Filter rather than counting Down presses: Ctrl+E lands on the search box, and
# a fixture that gains a file (.gitignore did) silently changes what N presses
# select. Filtering is also the honest way to reach a file in a real repo.
Ctrl+e
Sleep 900ms
Type "stats"
Sleep 1s
Down
Sleep 500ms
Enter
Sleep 1.5s

# jump to the divide and guard it
Type "/len(samples)"
Enter
Sleep 1.2s
Type "O"
Sleep 400ms
Type "    if not samples:"
Enter
Type "        return 0.0"
Escape
Sleep 900ms
Type ":w"
Enter
Sleep 1.5s

# --- 3. re-run: mean is fixed, and the same bug surfaces in spread ------
Escape
Sleep 600ms
Type "/terminal"
Sleep 400ms
Enter
Sleep 1.2s
Type "python3 report.py"
Sleep 300ms
Enter
Sleep 3.5s

# --- 4. hand the repeat to the agent ------------------------------------
Escape
Sleep 700ms
Type "spread() has the same empty-list bug. Fix it the same way, no need to run anything."
Sleep 600ms
Enter
Sleep 24s
