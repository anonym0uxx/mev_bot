//! Minimal `sources.yaml` reader — a deliberate YAML *subset*, not a YAML
//! library (§67: one dependency, and it is the HTTP client).
//!
//! The Python twins read `sources.yaml` through PyYAML; this module parses the
//! exact shapes that file uses (see `tools/social-ingest/sources.yaml`):
//! nested maps by two-space indentation, block lists of scalars, block lists
//! of flat maps (`- id: '...'` + aligned keys), single/double-quoted scalars,
//! full-line and trailing `#` comments. Anything outside that subset degrades
//! by being skipped — never a panic (§99-spirit) — matching the operator
//! contract: `sources.yaml` is operator-edited seed inventory, not hostile
//! input, but the capture edge still must not fall over on a typo.
//!
//! Pure `&str -> Yaml` (§22): file I/O stays with the callers so tests need no
//! filesystem.

/// One parsed YAML-subset node.
#[derive(Debug, Clone, PartialEq)]
pub enum Yaml {
    /// A scalar leaf (quotes stripped).
    Scalar(String),
    /// A block sequence.
    List(Vec<Yaml>),
    /// A block mapping (insertion order preserved).
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    /// Map member lookup; `None` on non-maps / missing keys (mirrors
    /// Python's `(data.get("x") or {})` chains via [`Yaml::get`] + defaults).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Scalar view.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Scalar(s) => Some(s),
            _ => None,
        }
    }

    /// List view.
    #[must_use]
    pub fn as_list(&self) -> Option<&[Yaml]> {
        match self {
            Yaml::List(items) => Some(items),
            _ => None,
        }
    }
}

/// Read + parse a sources file. A missing file yields an empty map — the
/// exact effect of the Python twins' `except FileNotFoundError` branches
/// (each caller's `or <default>` chain then supplies its own fallback).
/// Any other read error also degrades to empty: the capture edge keeps
/// running on its defaults rather than dying over an unreadable seed file.
#[must_use]
pub fn load_file(path: &str) -> Yaml {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Yaml::Map(Vec::new()),
    }
}

/// One preprocessed line: indent width (spaces) + comment-stripped content.
struct Line<'a> {
    indent: usize,
    content: &'a str,
}

/// Parse the YAML subset. Best-effort and total: every input yields a `Yaml`
/// (worst case an empty map), never a panic.
#[must_use]
pub fn parse(text: &str) -> Yaml {
    let lines: Vec<Line<'_>> = text
        .lines()
        .filter_map(|raw| {
            let stripped = strip_comment(raw);
            let trimmed = stripped.trim_end();
            let indent = trimmed.len() - trimmed.trim_start().len();
            let content = trimmed.trim_start();
            (!content.is_empty()).then_some(Line { indent, content })
        })
        .collect();
    let mut i = 0usize;
    let node = parse_block(&lines, &mut i, 0);
    node.unwrap_or(Yaml::Map(Vec::new()))
}

/// Cut a trailing comment: `#` at line start, or `#` preceded by whitespace,
/// outside single/double quotes.
fn strip_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_ws = true;
    for (idx, c) in line.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double && prev_ws => return &line[..idx],
            _ => {}
        }
        prev_ws = c.is_whitespace();
    }
    line
}

/// Strip one level of matching quotes; `''` inside single quotes unescapes.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return s[1..s.len() - 1].replace("\\\"", "\"");
    }
    s.to_string()
}

/// Does this content line open a mapping entry? YAML: `key: value` needs the
/// colon followed by a space (or line end) — which is what distinguishes
/// `channels:` from a bare URL scalar like `https://pump.fun/board`.
fn split_mapping(content: &str) -> Option<(&str, &str)> {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = content.as_bytes();
    for (idx, c) in content.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ':' if !in_single && !in_double => {
                let next = bytes.get(idx + 1);
                if next.is_none() || next == Some(&b' ') {
                    return Some((content[..idx].trim(), content[idx + 1..].trim()));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_block(lines: &[Line<'_>], i: &mut usize, indent: usize) -> Option<Yaml> {
    let first = lines.get(*i)?;
    if first.indent != indent {
        return None;
    }
    if first.content.starts_with('-') {
        parse_list(lines, i, indent)
    } else {
        parse_map(lines, i, indent)
    }
}

fn parse_map(lines: &[Line<'_>], i: &mut usize, indent: usize) -> Option<Yaml> {
    let mut pairs = Vec::new();
    while let Some(line) = lines.get(*i) {
        if line.indent > indent {
            *i += 1; // dangling deeper line (outside the subset): skip
            continue;
        }
        if line.indent < indent || line.content.starts_with('-') {
            break;
        }
        let Some((key, value)) = split_mapping(line.content) else {
            *i += 1; // outside the subset: skip, never fail
            continue;
        };
        *i += 1;
        if value.is_empty() {
            // Nested block (or an intentionally empty key).
            let child = match lines.get(*i) {
                Some(next) if next.indent > indent => parse_block(lines, i, next.indent),
                _ => None,
            };
            pairs.push((unquote(key), child.unwrap_or(Yaml::Scalar(String::new()))));
        } else {
            pairs.push((unquote(key), Yaml::Scalar(unquote(value))));
        }
    }
    Some(Yaml::Map(pairs))
}

fn parse_list(lines: &[Line<'_>], i: &mut usize, indent: usize) -> Option<Yaml> {
    let mut items = Vec::new();
    while let Some(line) = lines.get(*i) {
        if line.indent > indent {
            *i += 1; // dangling deeper line (outside the subset): skip
            continue;
        }
        if line.indent < indent || !line.content.starts_with('-') {
            break;
        }
        let rest = line.content[1..].trim_start();
        *i += 1;
        if let Some((key, value)) = split_mapping(rest) {
            // `- key: value` opens a flat map; aligned deeper `key: value`
            // lines continue it (the only list-of-maps shape sources.yaml
            // uses).
            let mut pairs = vec![(unquote(key), Yaml::Scalar(unquote(value)))];
            while let Some(next) = lines.get(*i) {
                if next.indent <= indent || next.content.starts_with('-') {
                    break;
                }
                let Some((k, v)) = split_mapping(next.content) else {
                    break;
                };
                pairs.push((unquote(k), Yaml::Scalar(unquote(v))));
                *i += 1;
            }
            items.push(Yaml::Map(pairs));
        } else {
            items.push(Yaml::Scalar(unquote(rest)));
        }
    }
    Some(Yaml::List(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A faithful cut of `tools/social-ingest/sources.yaml`.
    const SOURCES: &str = r#"
# Seed source inventory for social ingestion.

telegram:
  # real-time via MTProto
  channels:
    - crypticannouncements
    - chasescharts

x:
  firehose_query: '($SOL OR "pump.fun" OR "pumpfun" OR url:pump.fun) -is:retweet'
  amplifier_accounts:
    - blknoiz06      # Ansem — amplifier tier
    - OrangeSBS      # Orangie — amplifier tier
  lists:
    - id: '2074150651030876515'
      label: greek-ct-cluster

tiktok:
  hashtags:
    - solana
    - memecoin

web:
  pages:
    - https://www.dexscreener.com/solana
    - https://pump.fun/board
"#;

    fn strs(y: &Yaml) -> Vec<&str> {
        y.as_list()
            .unwrap()
            .iter()
            .filter_map(Yaml::as_str)
            .collect()
    }

    #[test]
    fn quoted_query_with_inner_double_quotes() {
        let root = parse(SOURCES);
        assert_eq!(
            root.get("x")
                .unwrap()
                .get("firehose_query")
                .unwrap()
                .as_str(),
            Some(r#"($SOL OR "pump.fun" OR "pumpfun" OR url:pump.fun) -is:retweet"#)
        );
    }

    #[test]
    fn scalar_lists_with_trailing_comments() {
        let root = parse(SOURCES);
        assert_eq!(
            strs(root.get("x").unwrap().get("amplifier_accounts").unwrap()),
            ["blknoiz06", "OrangeSBS"]
        );
        assert_eq!(
            strs(root.get("telegram").unwrap().get("channels").unwrap()),
            ["crypticannouncements", "chasescharts"]
        );
    }

    #[test]
    fn list_of_flat_maps() {
        let root = parse(SOURCES);
        let lists = root
            .get("x")
            .unwrap()
            .get("lists")
            .unwrap()
            .as_list()
            .unwrap();
        assert_eq!(lists.len(), 1);
        assert_eq!(
            lists[0].get("id").unwrap().as_str(),
            Some("2074150651030876515")
        );
        assert_eq!(
            lists[0].get("label").unwrap().as_str(),
            Some("greek-ct-cluster")
        );
    }

    #[test]
    fn url_scalars_are_not_mistaken_for_mappings() {
        let root = parse(SOURCES);
        assert_eq!(
            strs(root.get("web").unwrap().get("pages").unwrap()),
            [
                "https://www.dexscreener.com/solana",
                "https://pump.fun/board"
            ]
        );
    }

    #[test]
    fn hashtags_parse() {
        let root = parse(SOURCES);
        assert_eq!(
            strs(root.get("tiktok").unwrap().get("hashtags").unwrap()),
            ["solana", "memecoin"]
        );
    }

    #[test]
    fn junk_degrades_without_panic() {
        // Outside the subset: flow style, anchors — skipped, never a crash.
        let y = parse("a: [1, 2]\n&anchor\n  - x\nb: ok\n");
        assert_eq!(y.get("b").and_then(Yaml::as_str), Some("ok"));
        assert_eq!(parse(""), Yaml::Map(vec![]));
    }
}
