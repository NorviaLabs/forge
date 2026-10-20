# Source for .github/assets/demo.gif.
#
# The GIF used to be an orphan binary with no way to regenerate it, so it drifted:
# it still showed 0.1.0-beta.4 and a footer glyph that no longer exists. Keep this
# tape in step with the UI and re-record instead of hand-editing the asset.
#
# Regenerate:
#   cargo build --release
#   sh .github/assets/demo-fixture.sh
#   vhs .github/assets/demo.tape            # writes demo.mp4 (lossless source)
#   ffmpeg -i demo.mp4 -filter_complex "\
#     [0:v]trim=0:30.5,setpts=PTS-STARTPTS[a];\
#     [0:v]trim=30.5:48.5,setpts=(PTS-STARTPTS)/4[b];\
#     [0:v]trim=48.5,setpts=PTS-STARTPTS[c];\
#     [a][b][c]concat=n=3:v=1,fps=15,split[s0][s1];\
#     [s0]palettegen=max_colors=256:stats_mode=full[p];\
#     [s1][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
#     -loop 0 -y .github/assets/demo.gif && rm demo.mp4
#
# The middle trim plays the agent's "working" stretch at 4x. Its bounds depend
# on how long the live model call took, so scrub the MP4 and move them to just
# after the prompt is sent and just before the answer starts streaming. The
# turn's own "Response finished · Ns" line still reports the real duration.
# Check the last frame before committing — the demo has to end on report.py
# printing clean output, because the claim being made is verified code, not a
# diff that merely looks right.
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

Output demo.mp4

Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 15
Set LineHeight 1.2
Set Width 1400
Set Height 860
Set Padding 14
Set Margin 28
Set MarginFill "#0B0D10"
Set WindowBar Colorful
Set WindowBarSize 36
Set BorderRadius 10
Set Framerate 20
Set TypingSpeed 40ms
Set WaitTimeout 90s
Set Theme { "name": "forge-dark", "background": "#14171B", "foreground": "#D6DCE4", "cursor": "#86B5FF", "selection": "#26364D", "black": "#101318", "red": "#F2777A", "green": "#8FD6A0", "yellow": "#E6C07B", "blue": "#86B5FF", "magenta": "#C49BF0", "cyan": "#7FD1D9", "white": "#D6DCE4", "brightBlack": "#5C6773", "brightRed": "#F2777A", "brightGreen": "#8FD6A0", "brightYellow": "#E6C07B", "brightBlue": "#86B5FF", "brightMagenta": "#C49BF0", "brightCyan": "#7FD1D9", "brightWhite": "#FFFFFF" }

# Launch and the trust prompt are plumbing, not the story.
Hide
# ZDOTDIR (written by demo-fixture.sh) gives the embedded shell a neutral
# prompt, so the take does not publish whoever recorded it as user@host.
Type "cd /tmp/forge-demo-ws && export PYTHONDONTWRITEBYTECODE=1 ZDOTDIR=/tmp/forge-demo-ws-zdot SHELL=/bin/zsh && clear"
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
Wait+Screen /finished/
Sleep 3s

# --- 5. verify the agent's fix the way you verified your own ------------
# The point of the demo is verified code, not a diff that looks plausible, so
# it has to end on the program actually running clean.
Type "/terminal"
Sleep 400ms
Enter
Sleep 1.2s
Type "python3 report.py"
Sleep 300ms
Enter
Sleep 5s
