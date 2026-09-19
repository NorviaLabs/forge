//! Small, dependency-free Mermaid flowchart renderer for terminal previews.

use ratatui::text::{Line, Span};

use crate::theme;

#[derive(Clone)]
struct Node {
    id: String,
    label: String,
}

fn edge_part(raw: &str) -> Option<(String, Option<String>)> {
    let trimmed = raw.trim();
    let (without_label, label) = if let Some(stripped) = trimmed.strip_prefix('|') {
        let end = stripped.find('|')? + 1;
        (
            trimmed[end + 1..].trim().to_string(),
            Some(stripped[..end - 1].trim().to_string()),
        )
    } else {
        (trimmed.to_string(), None)
    };
    Some((node_ref(&without_label)?, label))
}

fn node_ref(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let id = trimmed
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .find(|part| !part.is_empty())?;
    Some(id.to_string())
}

fn node_label(raw: &str) -> Option<(String, String)> {
    let open = raw.find(['[', '(', '{'])?;
    let closing = match raw.as_bytes()[open] {
        b'[' => b']',
        b'(' => b')',
        b'{' => b'}',
        _ => return None,
    };
    let close = raw[open + 1..].find(closing as char)? + open + 1;
    if close <= open {
        return None;
    }
    let id = node_ref(&raw[..open])?;
    let label = raw[open + 1..close]
        .replace("<br/>", " ")
        .replace("<br>", " ")
        .replace('\\', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let fallback = id.clone();
    Some((id, if label.is_empty() { fallback } else { label }))
}

fn box_line(label: &str) -> Line<'static> {
    let width = label.chars().count() + 2;
    Line::from(vec![
        Span::styled(
            format!("╭{}╮", "─".repeat(width)),
            theme::response_heading(),
        ),
        Span::styled(format!("│ {label} │"), theme::text()),
        Span::styled(
            format!("╰{}╯", "─".repeat(width)),
            theme::response_heading(),
        ),
    ])
}

/// Render a Mermaid flowchart into terminal-friendly boxed rows.
///
/// Mermaid's browser renderer cannot run inside a ratatui process. This keeps
/// previews useful without a runtime, WebView, or native graphics dependency.
pub(crate) fn render(text: &str, width: usize) -> Option<Vec<Line<'static>>> {
    let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if !(first.starts_with("flowchart") || first.starts_with("graph")) {
        return None;
    }

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty()
            || line.starts_with("%%")
            || line.starts_with("subgraph")
            || line == "end"
            || line.starts_with("class")
            || line.starts_with("classDef")
        {
            continue;
        }
        let normalized = line.replace("-.->", "-->").replace("==>", "-->");
        if normalized.contains("-->") {
            let parts = normalized.split("-->").collect::<Vec<_>>();
            for pair in parts.windows(2) {
                if let (Some((from, _)), Some((to, label))) =
                    (edge_part(pair[0]), edge_part(pair[1]))
                {
                    edges.push((from, to, label));
                }
            }
        }
        if let Some((id, label)) = node_label(line) {
            if !nodes.iter().any(|existing: &Node| existing.id == id) {
                nodes.push(Node { id, label });
            }
        }
        for token in line.split_whitespace() {
            if let Some(node) = node_label(token) {
                if !nodes.iter().any(|existing: &Node| existing.id == node.0) {
                    nodes.push(Node {
                        id: node.0,
                        label: node.1,
                    });
                }
            }
        }
    }
    if nodes.is_empty() {
        return None;
    }

    let mut out = vec![Line::styled("MERMAID FLOWCHART", theme::response_heading())];
    let max_width = width.max(20);
    for (from, to, label) in edges {
        let from_label = nodes
            .iter()
            .find(|node| node.id == from)
            .map(|n| n.label.as_str());
        let to_label = nodes
            .iter()
            .find(|node| node.id == to)
            .map(|n| n.label.as_str());
        let (Some(from_label), Some(to_label)) = (from_label, to_label) else {
            continue;
        };
        let arrow = label
            .map(|label| format!("─ {label} ─▶"))
            .unwrap_or_else(|| "──▶".to_string());
        let plain = format!("[ {from_label} ]  {arrow}  [ {to_label} ]");
        if plain.chars().count() <= max_width {
            out.push(Line::styled(plain, theme::text()));
        } else {
            out.push(box_line(from_label));
            out.push(Line::styled("          │", theme::muted()));
            out.push(box_line(to_label));
        }
    }
    if out.len() == 1 {
        for node in nodes {
            out.push(box_line(&node.label));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_flowchart_edges() {
        let lines = render("flowchart TB\n  a[Start] --> b[Finish]", 80).unwrap();
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Start"));
        assert!(rendered.contains("Finish"));
        assert!(rendered.contains("▶"));
    }

    #[test]
    fn leaves_unsupported_diagrams_for_source_fallback() {
        assert!(render("sequenceDiagram\n  A->>B: hello", 80).is_none());
    }

    #[test]
    fn preserves_branch_targets_and_edge_labels() {
        let lines = render(
            "flowchart LR\n  start[Start] -->|Yes| success[Success]\n  start -->|No| failure[Failure]",
            120,
        )
        .unwrap();
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Start"));
        assert!(rendered.contains("Yes"));
        assert!(rendered.contains("Success"));
        assert!(rendered.contains("No"));
        assert!(rendered.contains("Failure"));
    }

    #[test]
    fn renders_fan_out_and_dotted_edges_without_merging_labels() {
        let lines = render(
            "flowchart LR\n  gate{Ready?} -->|Yes| a[First]\n  gate -.->|Telemetry| b[Metrics]",
            120,
        )
        .unwrap();
        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Ready?"));
        assert!(rendered.contains("Yes"));
        assert!(rendered.contains("First"));
        assert!(rendered.contains("Telemetry"));
        assert!(rendered.contains("Metrics"));
        assert!(!rendered.contains("Ready?} -->"));
    }
}
