<script>
  // A small, fixed icon set. Stroke-only, sized by the caller.
  let { name, size = 15 } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.7"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name === "chat"}
    <path d="M21 11.5a8.4 8.4 0 0 1-9 8.4 9 9 0 0 1-3.8-.8L3 21l1.9-4.6A8.4 8.4 0 1 1 21 11.5Z" />
  {:else if name === "files"}
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
  {:else if name === "file"}
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8Z" />
    <path d="M14 3v5h5" />
  {:else if name === "diff"}
    <circle cx="6" cy="6" r="2.2" />
    <circle cx="6" cy="18" r="2.2" />
    <circle cx="18" cy="8" r="2.2" />
    <path d="M6 8.2v7.6M18 10.2c0 4-4 3.8-9.2 5.4" />
  {:else if name === "plus"}
    <path d="M12 5v14M5 12h14" />
  {:else if name === "close"}
    <path d="M6 6l12 12M18 6 6 18" />
  {:else if name === "trash"}
    <path d="M4 7h16M9 7V5h6v2M6 7l1 13h10l1-13" />
  {:else if name === "refresh"}
    <path d="M20 12a8 8 0 1 1-2.3-5.7" />
    <path d="M20 4v5h-5" />
  {:else if name === "branch"}
    <circle cx="6" cy="6" r="2.2" />
    <circle cx="6" cy="18" r="2.2" />
    <circle cx="18" cy="6" r="2.2" />
    <path d="M6 8.2v7.6M18 8.2c0 4-4 3.8-9.2 5.4" />
  {:else if name === "send"}
    <path d="M12 19V5M6 11l6-6 6 6" />
  {:else if name === "stop"}
    <rect x="7" y="7" width="10" height="10" rx="1.5" />
  {:else if name === "terminal"}
    <rect x="3.5" y="5" width="17" height="14" rx="2" />
    <path d="M7 10l2.5 2L7 14M12.5 14.5h4.5" />
  {:else if name === "search"}
    <circle cx="11" cy="11" r="6.5" />
    <path d="M16 16l4.5 4.5" />
  {:else if name === "check"}
    <path d="M4 12.5 9 17.5 20 6.5" />
  {:else if name === "alert"}
    <path d="M12 4 2.8 20h18.4Z" />
    <path d="M12 10v4M12 17.2v.1" />
  {:else if name === "arrow-left"}
    <path d="M19 12H5M11 6l-6 6 6 6" />
  {:else if name === "more"}
    <circle cx="5" cy="12" r="1.4" />
    <circle cx="12" cy="12" r="1.4" />
    <circle cx="19" cy="12" r="1.4" />
  {:else if name === "arrow-up"}
    <path d="M12 19V5M6 11l6-6 6 6" />
  {:else if name === "arrow-down"}
    <path d="M12 5v14M6 13l6 6 6-6" />
  {:else if name === "pin"}
    <path d="M9 3h6l-1 5 3 3v2H7v-2l3-3Z" />
    <path d="M12 13v8" />
  {:else if name === "pencil"}
    <path d="M4 20h4L20 8l-4-4L4 16Z" />
    <path d="M14 6l4 4" />
  {:else if name === "play"}
    <path d="M7 4.5v15L20 12Z" />
  {:else if name === "layers"}
    <path d="M12 3 3 8l9 5 9-5Z" />
    <path d="M3 13l9 5 9-5" />
  {:else if name === "chevron"}
    <path d="M9 6l6 6-6 6" />
  {:else if name === "expand"}
    <path d="M14.25 3.75h6v6M9.75 20.25h-6v-6M20.25 3.75L13.5 10.5M3.75 20.25L10.5 13.5" />
  {/if}
</svg>
