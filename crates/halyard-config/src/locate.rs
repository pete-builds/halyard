//! Helpers that map TOML byte offsets and key paths back to source lines, so
//! errors can name the key and the line the user has to fix.

/// Strip a table header line to its dotted name: `[a.b]` and `[[a.b]]` give `a.b`.
fn header_name(line: &str) -> Option<&str> {
    let t = line.trim();
    let inner = t.strip_prefix("[[").and_then(|s| s.split("]]").next()).or_else(|| {
        t.strip_prefix('[').filter(|_| !t.starts_with("[[")).and_then(|s| s.split(']').next())
    })?;
    Some(inner.trim())
}

/// The key text on a `key = value` line, without quotes, if the line is one.
fn key_on_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    let (k, _) = t.split_once('=')?;
    let k = k.trim().trim_matches('"').trim_matches('\'').trim();
    if k.is_empty() || k.contains(['[', ']', '{', ',']) {
        return None;
    }
    Some(k.to_string())
}

/// 1-based line number of a byte offset.
pub(crate) fn line_of_offset(src: &str, offset: usize) -> usize {
    let end = offset.min(src.len());
    src.as_bytes()[..end].iter().filter(|b| **b == b'\n').count() + 1
}

/// The dotted key (`section.key`) that owns the given byte offset, plus its
/// 1-based line. For a value that spans several lines (an array) the key line
/// above is used. Returns `None` for the key when none can be found.
pub(crate) fn key_at_offset(src: &str, offset: usize) -> (Option<String>, usize) {
    let target = line_of_offset(src, offset);
    let lines: Vec<&str> = src.lines().collect();
    let mut section = String::new();
    let mut key: Option<(String, usize)> = None;
    for (i, line) in lines.iter().enumerate().take(target) {
        if let Some(h) = header_name(line) {
            section = h.to_string();
            key = None;
        } else if let Some(k) = key_on_line(line) {
            key = Some((k, i + 1));
        }
    }
    match key {
        Some((k, kline)) if section.is_empty() => (Some(k), kline),
        Some((k, kline)) => (Some(format!("{section}.{k}")), kline),
        None => (None, target),
    }
}

/// Find the 1-based line of `key` inside the `occurrence`-th table named
/// `section` (use 0 for ordinary tables; for `[[array]]` tables it picks the
/// element). An empty `section` means the top level.
pub(crate) fn find_key_line(
    src: &str,
    section: &str,
    occurrence: usize,
    key: &str,
) -> Option<usize> {
    let mut current = String::new();
    let mut seen_for_current = 0usize;
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if let Some(h) = header_name(line) {
            current = h.to_string();
            let c = counts.iter_mut().find(|(n, _)| *n == current);
            seen_for_current = match c {
                Some((_, n)) => {
                    *n += 1;
                    *n
                }
                None => {
                    counts.push((current.clone(), 0));
                    0
                }
            };
            continue;
        }
        if current == section
            && seen_for_current == occurrence
            && key_on_line(line).as_deref() == Some(key)
        {
            return Some(i + 1);
        }
    }
    None
}

/// 1-based line of a table header such as `[font]`, if present.
pub(crate) fn find_header_line(src: &str, section: &str, occurrence: usize) -> Option<usize> {
    let mut n = 0;
    for (i, line) in src.lines().enumerate() {
        if header_name(line) == Some(section) {
            if n == occurrence {
                return Some(i + 1);
            }
            n += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "top = 1\n[font]\nsize = \"big\"\n[[ai.providers]]\nname = \"a\"\n[[ai.providers]]\nname = \"b\"\nkind = [\n  \"x\",\n]\n[keybindings]\n\"cmd+t\" = \"new_tab\"\n";

    #[test]
    fn key_at_offset_names_section_and_key() {
        let off = SRC.find("\"big\"").unwrap();
        assert_eq!(key_at_offset(SRC, off), (Some("font.size".into()), 3));
    }

    #[test]
    fn key_at_offset_walks_up_from_array_continuation() {
        let off = SRC.find("\"x\"").unwrap();
        assert_eq!(key_at_offset(SRC, off), (Some("ai.providers.kind".into()), 8));
    }

    #[test]
    fn find_key_line_picks_array_element() {
        assert_eq!(find_key_line(SRC, "ai.providers", 0, "name"), Some(5));
        assert_eq!(find_key_line(SRC, "ai.providers", 1, "name"), Some(7));
        assert_eq!(find_key_line(SRC, "", 0, "top"), Some(1));
        assert_eq!(find_key_line(SRC, "keybindings", 0, "cmd+t"), Some(12));
        assert_eq!(find_key_line(SRC, "font", 0, "nope"), None);
    }

    #[test]
    fn header_line_found() {
        assert_eq!(find_header_line(SRC, "font", 0), Some(2));
        assert_eq!(find_header_line(SRC, "ai.providers", 1), Some(6));
    }
}
