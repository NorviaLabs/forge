<script>
  import { onDestroy, onMount } from "svelte";
  import { Channel, invoke } from "@tauri-apps/api/core";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";

  let { sessionId = "", themeMode = "dark" } = $props();

  let host = $state();
  let terminal = null;
  let fit = null;
  let observer = null;
  let disposed = false;

  function terminalTheme() {
    const style = getComputedStyle(document.documentElement);
    const color = (name) => style.getPropertyValue(`--${name}`).trim();
    return {
      background: color("bg-deep"), foreground: color("secondary"), cursor: color("accent"),
      cursorAccent: color("bg-deep"), selectionBackground: color("selection"),
      black: color("bg"), red: color("error"), green: color("success"), yellow: color("warning"),
      blue: color("accent"), magenta: color("strong"), cyan: color("accent"), white: color("text"),
      brightBlack: color("muted"), brightRed: color("error"), brightGreen: color("success"),
      brightYellow: color("warning"), brightBlue: color("accent"), brightMagenta: color("strong"),
      brightCyan: color("accent"), brightWhite: color("text"),
    };
  }
  $effect(() => {
    void themeMode;
    if (terminal) terminal.options.theme = terminalTheme();
  });

  function decode(data) {
    const binary = atob(data);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  }

  onMount(async () => {
    terminal = new Terminal({
      theme: terminalTheme(),
      fontFamily:
        'ui-monospace, "SF Mono", "JetBrains Mono", Menlo, monospace',
      fontSize: 12,
      lineHeight: 1.4,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: true,
      macOptionIsMeta: true,
    });
    fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(host);
    fit.fit();
    terminal.focus();

    terminal.onData((data) => {
      invoke("shell_write", { sessionId, data }).catch(() => {});
    });

    const channel = new Channel();
    channel.onmessage = (event) => {
      if (event.type === "output") {
        terminal?.write(decode(event.data));
      } else if (event.type === "exit") {
        terminal?.write("\r\n[shell exited]\r\n");
      }
    };

    try {
      await invoke("shell_open", {
        channel,
        sessionId,
        cols: terminal.cols,
        rows: terminal.rows,
      });
    } catch (error) {
      terminal?.write(`\r\n[cannot open a shell: ${error}]\r\n`);
    }

    if (disposed) {
      await invoke("shell_close", { sessionId }).catch(() => {});
      return;
    }
    observer = new ResizeObserver(() => {
      if (!fit || disposed || !host.clientWidth || !host.clientHeight) return;
      fit.fit();
      invoke("shell_resize", { sessionId, cols: terminal.cols, rows: terminal.rows }).catch(
        () => {},
      );
    });
    observer.observe(host);
  });

  onDestroy(() => {
    disposed = true;
    observer?.disconnect();
    invoke("shell_close", { sessionId }).catch(() => {});
    terminal?.dispose();
    terminal = null;
  });
</script>

<div class="terminal" bind:this={host} data-session={sessionId}></div>

<style>
  .terminal {
    flex: 1;
    min-height: 0;
    padding: 8px 0 10px 12px;
    background: var(--bg-deep);
  }

  .terminal :global(.xterm) {
    height: 100%;
  }

  .terminal :global(.xterm-viewport) {
    background: transparent !important;
  }
</style>
