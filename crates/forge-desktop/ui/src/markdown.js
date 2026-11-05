// Model output is untrusted. Markdown is rendered with raw HTML off, with
// links, autolinks and images disabled (they render as text: the webview
// never navigates or loads from model content), and the result passes
// through DOMPurify once more before the single raw-HTML insertion in
// Markdown.svelte.
import MarkdownIt from "markdown-it";
import DOMPurify from "dompurify";

const md = new MarkdownIt({ html: false, linkify: false, typographer: false });
md.disable(["link", "image", "autolink"]);

const PURIFY = {
  FORBID_TAGS: ["style", "script", "iframe", "frame", "object", "embed", "form", "img", "svg", "math", "link", "meta", "base"],
  FORBID_ATTR: ["style", "href", "src", "srcset", "action", "formaction"],
  ALLOW_DATA_ATTR: false,
};

export function renderMarkdown(text) {
  return DOMPurify.sanitize(md.render(text ?? ""), PURIFY);
}
