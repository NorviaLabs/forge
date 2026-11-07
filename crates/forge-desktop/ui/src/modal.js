// Keep keyboard focus inside overlays, and return it to their launcher.
export function modal(node, options) {
  const previous = document.activeElement;
  const focusable = () => [...node.querySelectorAll('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]')].filter((element) => element.getClientRects().length);
  (focusable()[0] ?? node).focus();
  function keydown(event) {
    if (event.key === "Escape") {
      event.stopPropagation();
      if (!options.busy) options.onClose?.();
    } else if (event.key === "Tab") {
      const elements = focusable();
      const index = elements.indexOf(document.activeElement);
      if (!elements.length || (event.shiftKey && index <= 0) || (!event.shiftKey && (index === elements.length - 1 || index < 0))) {
        event.preventDefault();
        (event.shiftKey ? elements.at(-1) : elements[0])?.focus();
      }
    }
  }
  node.addEventListener("keydown", keydown);
  return {
    update(value) { options = value; },
    destroy() { node.removeEventListener("keydown", keydown); if (previous?.isConnected) previous.focus(); },
  };
}
