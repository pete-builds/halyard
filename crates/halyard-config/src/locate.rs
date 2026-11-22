//! Source positions, taken from the TOML parser's own spans and never from scanning lines.
//!
//! A line scanner can be fooled by quoted headers, dotted keys and multi-line strings, so
//! nothing here (or anywhere else in the crate) decides what a piece of text IS by looking
//! at lines. [`Index`] walks the parsed document and records, for every key, its dotted
//! path and the line its name starts on.

use std::collections::HashMap;
use std::ops::Range;

use toml::de::{DeTable, DeValue};

/// 1-based line number of a byte offset.
pub(crate) fn line_of_offset(src: &str, offset: usize) -> usize {
    let end = offset.min(src.len());
    src.as_bytes()[..end].iter().filter(|b| **b == b'\n').count() + 1
}

/// The placeholder shown instead of a key name that must not be echoed.
pub(crate) const HIDDEN: &str = "<hidden>";

/// Which part of `[ai]` a table is, which decides whether key names may be shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Plain,
    AiRoot,
    AiProvider,
    AiHidden,
}

/// Key paths and lines for one parsed document.
///
/// Under `[ai]`, any key Halyard does not define is shown as [`HIDDEN`] in paths, because
/// an unknown name there may itself be a pasted secret.
pub(crate) struct Index {
    lines: HashMap<String, usize>,
    spans: Vec<(Range<usize>, String)>,
}

impl Index {
    /// Build the index. `redact_ai` turns on the `[ai]` name hiding (config files only).
    pub(crate) fn build(src: &str, table: &DeTable<'_>, redact_ai: bool) -> Self {
        let mut ix = Self { lines: HashMap::new(), spans: Vec::new() };
        ix.walk_table(src, table, "", Ctx::Plain, redact_ai, true);
        ix
    }

    fn add(&mut self, path: &str, line: usize, span: Range<usize>) {
        self.lines.entry(path.to_string()).or_insert(line);
        self.spans.push((span, path.to_string()));
    }

    fn walk_table(
        &mut self,
        src: &str,
        table: &DeTable<'_>,
        prefix: &str,
        ctx: Ctx,
        redact: bool,
        top: bool,
    ) {
        for (k, v) in table {
            let name = k.get_ref().as_ref();
            let hidden = (HIDDEN.to_string(), Ctx::AiHidden);
            let (seg, child) = if !redact {
                (name.to_string(), Ctx::Plain)
            } else {
                match ctx {
                    Ctx::Plain if top && name == "ai" => ("ai".to_string(), Ctx::AiRoot),
                    Ctx::Plain => (name.to_string(), Ctx::Plain),
                    Ctx::AiRoot => match name {
                        "providers" => (name.to_string(), Ctx::AiProvider),
                        "enabled" => (name.to_string(), Ctx::AiHidden),
                        _ => hidden,
                    },
                    Ctx::AiProvider => {
                        if matches!(name, "name" | "kind" | "base_url" | "model" | "api_key_env") {
                            (name.to_string(), Ctx::AiHidden)
                        } else {
                            hidden
                        }
                    }
                    Ctx::AiHidden => hidden,
                }
            };
            let path = if prefix.is_empty() { seg } else { format!("{prefix}.{seg}") };
            let (ks, vs) = (k.span(), v.span());
            self.add(&path, line_of_offset(src, ks.start), ks.start..vs.end.max(ks.end));
            self.walk_value(src, v.get_ref(), &path, child, redact);
        }
    }

    fn walk_value(&mut self, src: &str, v: &DeValue<'_>, path: &str, ctx: Ctx, redact: bool) {
        match v {
            DeValue::Table(t) => self.walk_table(src, t, path, ctx, redact, false),
            DeValue::Array(a) => {
                for (i, item) in a.iter().enumerate() {
                    let p = format!("{path}[{i}]");
                    let s = item.span();
                    self.add(&p, line_of_offset(src, s.start), s.clone());
                    self.walk_value(src, item.get_ref(), &p, ctx, redact);
                }
            }
            _ => {}
        }
    }

    /// Line of `path`, or of its nearest ancestor that has one.
    pub(crate) fn line(&self, path: &str) -> Option<usize> {
        let mut p = path;
        loop {
            if let Some(l) = self.lines.get(p) {
                return Some(*l);
            }
            p = &p[..p.rfind(['.', '['])?];
        }
    }

    /// Whether `path` exists in the document.
    pub(crate) fn has(&self, path: &str) -> bool {
        self.lines.contains_key(path)
    }

    /// The path of the innermost key or array item whose text contains `offset`.
    pub(crate) fn path_at(&self, offset: usize) -> Option<String> {
        self.spans
            .iter()
            .filter(|(s, _)| s.start <= offset && offset <= s.end)
            .min_by_key(|(s, _)| s.end - s.start)
            .map(|(_, p)| p.clone())
    }
}

/// Parse `src` into a spanned document.
pub(crate) fn parse_doc(src: &str) -> Result<toml::de::DeTable<'_>, ParseFailure> {
    DeTable::parse(src).map(toml::Spanned::into_inner).map_err(|e| ParseFailure {
        line: e.span().map(|s| line_of_offset(src, s.start)),
        message: e.message().to_string(),
    })
}

/// A TOML syntax error: the parser's own fixed description, with no source text in it.
pub(crate) struct ParseFailure {
    pub(crate) line: Option<usize>,
    pub(crate) message: String,
}

/// A serde error turned into text that holds no value from the file. serde and toml quote the
/// offending value in `invalid type`, `invalid value` and `unknown variant` messages, so only
/// a few fixed shapes are recognised and everything else becomes one generic sentence.
pub(crate) fn value_free(message: &str) -> String {
    if message.starts_with("missing field `") {
        // `missing field` names a field Halyard defines, never user text.
        return message.to_string();
    }
    if message.starts_with("unknown field `") {
        return "unknown key".to_string();
    }
    "invalid value for this key (the value is not shown; see docs/configuration.md for what it accepts)"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "[font]\nsize = 3\n\n[[ai.providers]]\nname = \"x\"\nsecret_thing = 1\n";

    #[test]
    fn index_has_lines_and_hides_unknown_ai_names() {
        let doc = parse_doc(SRC).ok().unwrap();
        let ix = Index::build(SRC, &doc, true);
        assert_eq!(ix.line("font.size"), Some(2));
        assert_eq!(ix.line("ai.providers[0].name"), Some(5));
        assert!(!ix.has("ai.providers[0].secret_thing"));
        assert_eq!(ix.line("ai.providers[0].<hidden>"), Some(6));
    }

    #[test]
    fn quoted_headers_and_dotted_keys_resolve_structurally() {
        let src = "[\"ai\"]\n\"enabled\" = true\n[[\"ai\".providers]]\nname = \"a\"\n";
        let doc = parse_doc(src).ok().unwrap();
        let ix = Index::build(src, &doc, true);
        assert_eq!(ix.line("ai.enabled"), Some(2));
        assert_eq!(ix.line("ai.providers[0].name"), Some(4));
        let src = "\"ai\".enabled = 1\n";
        let doc = parse_doc(src).ok().unwrap();
        assert_eq!(Index::build(src, &doc, true).line("ai.enabled"), Some(1));
    }

    #[test]
    fn a_header_look_alike_inside_a_multiline_string_changes_nothing() {
        let src = "[ai]\nenabled = true\nx = \"\"\"\n[font]\nenabled = 1\n\"\"\"\n";
        let doc = parse_doc(src).ok().unwrap();
        let ix = Index::build(src, &doc, true);
        assert!(!ix.has("font.enabled"));
        let off = src.find("enabled = 1").unwrap();
        assert_eq!(ix.path_at(off).as_deref(), Some("ai.<hidden>"));
    }
}
