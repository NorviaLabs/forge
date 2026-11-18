// Forge's view of a page. Evaluated in an isolated world: the page's own
// scripts cannot see this object, replace it, or read the refs it hands out.
// Refs are stable for an element for as long as the document lives.
(() => {
  if (globalThis.__forge) return true;

  const refs = new Map(); // "e12" -> WeakRef(element)
  const ids = new WeakMap(); // element -> "e12"
  let next = 1;

  const INTERACTIVE = [
    "a[href]", "button", "input:not([type=hidden])", "select", "textarea", "summary",
    "[role=button]", "[role=link]", "[role=checkbox]", "[role=radio]", "[role=tab]",
    "[role=menuitem]", "[role=option]", "[role=switch]", "[role=textbox]", "[role=combobox]",
    "[role=searchbox]", "[role=slider]", "[contenteditable=''], [contenteditable=true]",
    "[onclick]", "[tabindex]:not([tabindex='-1'])",
  ].join(",");

  const flat = (text, max) => {
    const value = String(text ?? "").replace(/\s+/g, " ").trim();
    return value.length > max ? value.slice(0, max) : value;
  };

  const refOf = (element) => {
    let ref = ids.get(element);
    if (!ref) {
      ref = `e${next++}`;
      ids.set(element, ref);
      refs.set(ref, new WeakRef(element));
    }
    return ref;
  };

  const resolve = (ref) => {
    const element = refs.get(ref)?.deref();
    return element && element.isConnected ? element : null;
  };

  const visible = (element) => {
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    const style = getComputedStyle(element);
    return style.visibility !== "hidden" && style.display !== "none" && Number(style.opacity) !== 0;
  };

  const nearViewport = (element) => {
    const rect = element.getBoundingClientRect();
    const margin = innerHeight / 2;
    return rect.bottom >= -margin && rect.top <= innerHeight + margin
      && rect.right >= 0 && rect.left <= innerWidth;
  };

  const inputType = (element) => (element.getAttribute("type") || "text").toLowerCase();

  const role = (element) => {
    const explicit = element.getAttribute("role");
    if (explicit) return explicit.split(/\s+/)[0];
    switch (element.tagName) {
      case "A": return "link";
      case "BUTTON": case "SUMMARY": return "button";
      case "SELECT": return element.multiple ? "listbox" : "combobox";
      case "TEXTAREA": return "textbox";
      case "INPUT": {
        const type = inputType(element);
        if (type === "checkbox") return "checkbox";
        if (type === "radio") return "radio";
        if (["submit", "button", "reset", "image"].includes(type)) return "button";
        if (type === "range") return "slider";
        if (type === "file") return "file";
        return "textbox";
      }
      default:
        return element.isContentEditable ? "textbox" : "clickable";
    }
  };

  // A label's own words, without the text of the controls inside it.
  const labelText = (label) => {
    let text = "";
    const walk = (node) => {
      for (const child of node.childNodes) {
        if (child.nodeType === Node.TEXT_NODE) text += child.textContent + " ";
        else if (child.nodeType === Node.ELEMENT_NODE
          && !["INPUT", "SELECT", "TEXTAREA", "BUTTON"].includes(child.tagName)) walk(child);
      }
    };
    walk(label);
    return text;
  };

  const name = (element) => {
    const aria = element.getAttribute("aria-label");
    if (aria) return flat(aria, 120);
    const labelledBy = element.getAttribute("aria-labelledby");
    if (labelledBy) {
      const text = labelledBy.split(/\s+/)
        .map((id) => document.getElementById(id)?.innerText ?? "").join(" ");
      if (text.trim()) return flat(text, 120);
    }
    if (element.labels && element.labels.length) {
      const text = [...element.labels].map(labelText).join(" ");
      if (text.trim()) return flat(text, 120);
    }
    if (element.tagName === "INPUT" && ["submit", "button", "reset"].includes(inputType(element))) {
      return flat(element.value || element.getAttribute("type"), 120);
    }
    const placeholder = element.getAttribute("placeholder");
    if (placeholder) return flat(placeholder, 120);
    const text = flat(element.innerText || element.textContent, 120);
    if (text) return text;
    const title = element.getAttribute("title");
    if (title) return flat(title, 120);
    const img = element.querySelector?.("img[alt]");
    if (img) return flat(img.getAttribute("alt"), 120);
    if (element.tagName === "INPUT" && element.name) return flat(element.name, 120);
    return "";
  };

  const submits = (element) => {
    if (element.tagName === "BUTTON") {
      const type = (element.getAttribute("type") || "submit").toLowerCase();
      return type === "submit" && !!element.form;
    }
    if (element.tagName === "INPUT") {
      return ["submit", "image"].includes(inputType(element)) && !!element.form;
    }
    return false;
  };

  const isPassword = (element) => element.tagName === "INPUT" && inputType(element) === "password";

  // Typing Enter here submits its form (implicit submission).
  const enterSubmits = (element) => {
    if (!element) return false;
    if (submits(element)) return true;
    if (element.tagName !== "INPUT" || !element.form) return false;
    return !["checkbox", "radio", "button", "reset", "file", "range", "color"].includes(inputType(element));
  };

  const describe = (element) => {
    const info = {
      ref: refOf(element),
      role: role(element),
      name: name(element),
      submits: submits(element),
      password: isPassword(element),
      disabled: !!element.disabled || element.getAttribute("aria-disabled") === "true",
      options: [],
    };
    if (element.tagName === "SELECT") {
      info.value = element.value;
      info.options = [...element.options].map((option) => flat(option.label || option.text, 60));
    } else if (element.tagName === "INPUT") {
      const type = inputType(element);
      if (type === "checkbox" || type === "radio") info.checked = element.checked;
      else if (!info.password && !["submit", "button", "reset", "image", "file"].includes(type)) {
        info.value = element.value;
      }
    } else if (element.tagName === "TEXTAREA") {
      info.value = flat(element.value, 500);
    } else if (element.isContentEditable) {
      info.value = flat(element.innerText, 500);
    }
    const checked = element.getAttribute("aria-checked");
    if (checked !== null && info.checked === undefined) info.checked = checked === "true";
    if (element.tagName === "A" && element.href) info.href = element.href;
    return info;
  };

  const BLOCK = /^(ADDRESS|ARTICLE|ASIDE|BLOCKQUOTE|DD|DIV|DL|DT|FIELDSET|FIGCAPTION|FIGURE|FOOTER|FORM|H[1-6]|HEADER|HR|LI|MAIN|NAV|OL|P|PRE|SECTION|TABLE|TD|TH|TR|UL|BODY|LABEL)$/;
  const blockOf = (node) => {
    let element = node.parentElement;
    while (element && !BLOCK.test(element.tagName)) element = element.parentElement;
    return element;
  };

  const box = (element) => {
    element.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return null;
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  };

  const formOf = (element) => {
    if (element.tagName === "FORM") return element;
    return element.form || element.closest("form");
  };

  globalThis.__forge = {
    // One pass over what is on screen: a subtree whose box is off screen
    // (beyond half a screen) is skipped whole, so a long page costs what is
    // visible, not its length. Positioned descendants of such a box are the
    // rare exception this does not read.
    snapshot(maxElements, maxChars) {
      const margin = innerHeight / 2;
      const root = document.body || document.documentElement;
      const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "OPTION", "HEAD"]);
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
        acceptNode(node) {
          if (node.nodeType === Node.TEXT_NODE) return NodeFilter.FILTER_ACCEPT;
          if (SKIP.has(node.tagName)) return NodeFilter.FILTER_REJECT;
          const rect = node.getBoundingClientRect();
          if (rect.width === 0 && rect.height === 0) {
            // No box: hidden (skip it all) or `display: contents` (look in).
            return getComputedStyle(node).display === "none"
              ? NodeFilter.FILTER_REJECT
              : NodeFilter.FILTER_ACCEPT;
          }
          // Off screen by more than half a screen: nothing inside it shows.
          if (rect.bottom < -margin || rect.top > innerHeight + margin) return NodeFilter.FILTER_REJECT;
          return NodeFilter.FILTER_ACCEPT;
        },
      });
      const elements = [];
      const range = document.createRange();
      let text = "";
      let lastBlock = null;
      let elementsFull = false;
      let textFull = false;
      let truncated = false;
      for (let node = walker.nextNode(); node && !(elementsFull && textFull); node = walker.nextNode()) {
        if (node.nodeType === Node.ELEMENT_NODE) {
          if (elementsFull || !node.matches(INTERACTIVE)) continue;
          if (!nearViewport(node) || !visible(node)) continue;
          if (elements.length >= maxElements) { elementsFull = true; truncated = true; continue; }
          elements.push(describe(node));
          continue;
        }
        if (textFull) continue;
        const value = flat(node.textContent, 2000);
        if (!value) continue;
        const parent = node.parentElement;
        if (!parent) continue;
        range.selectNodeContents(node);
        const rect = range.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) continue;
        if (rect.bottom < 0 || rect.top > innerHeight || rect.right < 0 || rect.left > innerWidth) continue;
        if (getComputedStyle(parent).visibility === "hidden") continue;
        const block = blockOf(node);
        text += text && block !== lastBlock ? "\n" : text ? " " : "";
        text += value;
        lastBlock = block;
        if (text.length > maxChars) {
          text = text.slice(0, maxChars);
          textFull = true;
          truncated = true;
        }
      }
      const scroller = document.scrollingElement || document.documentElement;
      return {
        url: location.href,
        title: document.title,
        elements,
        text,
        truncated,
        scroll_y: Math.round(scroller.scrollTop),
        scroll_height: Math.round(scroller.scrollHeight),
      };
    },

    // Where to click the element, after scrolling it into view, and what
    // else is on top there (a cover, another site's frame) and would get
    // the click instead.
    target(ref) {
      const element = resolve(ref);
      if (!element) return { missing: true };
      const info = describe(element);
      const point = box(element);
      let covered = null;
      if (point) {
        const hit = document.elementFromPoint(point.x, point.y);
        const label = hit?.closest?.("label");
        const ownLabel = !!label && (label.control === element || label.contains(element));
        if (!hit || (hit !== element && !element.contains(hit) && !ownLabel)) {
          const cover = hit ? (hit.closest("button, input, a[href], select, textarea, iframe, [role]") || hit) : null;
          covered = cover
            ? { ...describe(cover), name: describe(cover).name || cover.tagName.toLowerCase() }
            : { ref: "", role: "nothing", name: "the edge of the page" };
        }
      }
      return { ...info, point, visible: !!point && visible(element), covered };
    },

    // What a click at (x, y) would land on.
    at(x, y) {
      let element = document.elementFromPoint(x, y);
      if (!element) return null;
      element = element.closest("button, input, a[href], select, textarea, [role]") || element;
      return describe(element);
    },

    focused() {
      const element = document.activeElement;
      if (!element || element === document.body) return null;
      return { ...describe(element), enter_submits: enterSubmits(element) };
    },

    focus(ref, clear) {
      const element = resolve(ref);
      if (!element) return { missing: true };
      element.focus();
      if (clear) {
        if (typeof element.select === "function") element.select();
        else if (element.isContentEditable) {
          const range = document.createRange();
          range.selectNodeContents(element);
          const selection = getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
        }
      } else if (typeof element.setSelectionRange === "function") {
        try {
          const end = element.value.length;
          element.setSelectionRange(end, end);
        } catch (_) { /* types without a selection */ }
      }
      return { ...describe(element), focused: document.activeElement === element };
    },

    select(ref, wanted) {
      const element = resolve(ref);
      if (!element) return { missing: true };
      if (element.tagName !== "SELECT") return { error: "not a select element" };
      const want = String(wanted).toLowerCase();
      const option = [...element.options].find((candidate) => candidate.value.toLowerCase() === want)
        || [...element.options].find((candidate) => flat(candidate.label || candidate.text, 200).toLowerCase() === want);
      if (!option) {
        return { error: "no such option", options: [...element.options].map((o) => flat(o.label || o.text, 60)) };
      }
      element.focus();
      element.value = option.value;
      element.dispatchEvent(new Event("input", { bubbles: true }));
      element.dispatchEvent(new Event("change", { bubbles: true }));
      return { value: element.value, label: flat(option.label || option.text, 60) };
    },

    form(ref) {
      const element = resolve(ref);
      if (!element) return { missing: true };
      const form = formOf(element);
      if (!form) return { error: "not in a form" };
      const submitter = submits(element) ? element : null;
      const fields = [];
      for (const field of form.elements) {
        if (!field.name || field.disabled) continue;
        const type = field.tagName === "INPUT" ? inputType(field) : field.tagName.toLowerCase();
        if (["submit", "button", "reset", "image"].includes(type) && field !== submitter) continue;
        if ((type === "checkbox" || type === "radio") && !field.checked) continue;
        let value = type === "file"
          ? [...(field.files || [])].map((file) => file.name).join(", ") || "(no file)"
          : field.value;
        fields.push({ name: field.name, value: flat(value, 200), password: type === "password" });
      }
      const label = submitter ? name(submitter)
        : name(form.querySelector("button:not([type=button]):not([type=reset]), input[type=submit]") || form)
          || form.getAttribute("name") || "the form";
      return {
        label,
        page_url: location.href,
        method: (submitter?.getAttribute("formmethod") || form.method || "get").toLowerCase(),
        // A button's `formAction` is the page's URL when it sets none: only
        // an explicit `formaction` overrides where the form sends its data.
        action: (submitter?.hasAttribute("formaction") ? submitter.formAction : form.action) || location.href,
        fields,
        submitter_ref: submitter ? ref : null,
      };
    },

    // Submit the form `ref` is in, as the browser would for its submit
    // button (validation and submit handlers included).
    submit(ref) {
      const element = resolve(ref);
      if (!element) return { missing: true };
      const form = formOf(element);
      if (!form) return { error: "not in a form" };
      form.requestSubmit(submits(element) ? element : undefined);
      return { ok: true };
    },
  };
  return true;
})();
