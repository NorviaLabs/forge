// Literal, case-insensitive search with wraparound. Offsets stay UTF-16,
// matching textarea selections even when the file contains emoji.
export function findText(text, query, from = 0, backwards = false) {
  if (!query) return null;
  const pattern = new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "giu");
  const matches = [...text.matchAll(pattern)];
  const match = backwards
    ? matches.findLast((entry) => entry.index <= from) ?? matches.at(-1)
    : matches.find((entry) => entry.index >= from) ?? matches[0];
  return match ? [match.index, match.index + match[0].length] : null;
}

// Replace every literal match (same case-insensitive rule as findText).
export function replaceAllText(text, query, replacement) {
  if (!query) return { text, count: 0 };
  const pattern = new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "giu");
  let count = 0;
  const next = text.replace(pattern, () => {
    count += 1;
    return replacement;
  });
  return { text: next, count };
}

export function lineOffset(text, line) {
  if (!Number.isSafeInteger(line) || line < 1) return null;
  let offset = 0;
  for (let index = 1; index < line; index += 1) {
    const newline = text.indexOf("\n", offset);
    if (newline < 0) return null;
    offset = newline + 1;
  }
  return offset;
}

const keywords = new Set("as async await break case catch class const continue def default do else enum export extends false finally fn for from function if impl import in interface let match mod move mut new None null of pass pub raise ref return self static struct super switch this throw trait true try type typeof undefined use var void where while with yield".split(" "));

// shortcut: lexical colors for common code/config files, not a language
// parser; use a grammar-backed editor if semantic highlighting is needed.
export function highlight(text, path) {
  const extension = path?.split(".").pop()?.toLowerCase();
  const cLike = ["rs", "js", "jsx", "ts", "tsx", "c", "h", "cpp", "cs", "java", "go", "swift", "css", "json"].includes(extension);
  const hash = ["py", "sh", "bash", "zsh", "rb", "yaml", "yml", "toml"].includes(extension);
  if ((!cLike && !hash) || text.length > 65536) return [{ text, kind: "" }];
  const pattern = cLike
    ? /\/\/[^\n]*|\/\*[\s\S]*?(?:\*\/|$)|"(?:\\[\s\S]|[^"\\])*"|'(?:\\[\s\S]|[^'\\])*'|`(?:\\[\s\S]|[^`\\])*`|\b\d+(?:\.\d+)?\b|\b[A-Za-z_]\w*\b/g
    : /#[^\n]*|"""[\s\S]*?(?:"""|$)|'''[\s\S]*?(?:'''|$)|"(?:\\[\s\S]|[^"\\])*"|'(?:\\[\s\S]|[^'\\])*'|\b\d+(?:\.\d+)?\b|\b[A-Za-z_]\w*\b/g;
  const tokens = [];
  let end = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > end) tokens.push({ text: text.slice(end, match.index), kind: "" });
    const word = match[0];
    const kind = word.startsWith("//") || word.startsWith("/*") || (hash && word.startsWith("#"))
      ? "comment" : /^["'`]/.test(word) ? "string" : /^\d/.test(word) ? "number" : keywords.has(word) ? "keyword" : "";
    tokens.push({ text: word, kind });
    end = match.index + word.length;
  }
  if (end < text.length) tokens.push({ text: text.slice(end), kind: "" });
  return tokens;
}
