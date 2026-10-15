//! Viewport-first incremental parsing with explicit large-file cutoffs.

use tree_sitter::{Parser, Tree};

pub const LARGE_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseMode {
    Full,
    Reduced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxSpan {
    pub start: usize,
    pub end: usize,
    pub token: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: String,
    pub start: usize,
}

pub struct SyntaxDocument {
    parser: Parser,
    tree: Option<Tree>,
    mode: ParseMode,
}

impl SyntaxDocument {
    /// Creates the maintained Rust language pack.
    /// # Errors
    /// Returns an error if the bundled grammar is ABI-incompatible.
    pub fn rust(source_len: usize) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        Ok(Self {
            parser,
            tree: None,
            mode: if source_len > LARGE_FILE_BYTES {
                ParseMode::Reduced
            } else {
                ParseMode::Full
            },
        })
    }

    #[must_use]
    pub const fn mode(&self) -> ParseMode {
        self.mode
    }

    pub fn parse(&mut self, source: &str) -> bool {
        if self.mode == ParseMode::Reduced {
            self.tree = None;
            return false;
        }
        self.tree = self.parser.parse(source, self.tree.as_ref());
        self.tree.is_some()
    }

    #[must_use]
    pub fn visible_nodes(&self, start_byte: usize, end_byte: usize) -> Vec<(usize, usize, String)> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut cursor = tree.walk();
        let mut output = Vec::new();
        loop {
            let node = cursor.node();
            if node.end_byte() >= start_byte && node.start_byte() <= end_byte {
                output.push((node.start_byte(), node.end_byte(), node.kind().to_owned()));
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return output;
                }
            }
        }
    }

    #[must_use]
    pub fn highlight(&self, start: usize, end: usize) -> Vec<SyntaxSpan> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut cursor = tree.walk();
        let mut spans = Vec::new();
        loop {
            let node = cursor.node();
            if node.end_byte() >= start && node.start_byte() <= end {
                let span = match node.kind() {
                    "line_comment" | "block_comment" => Some((node, "comment")),
                    "string_literal" | "raw_string_literal" | "char_literal" => {
                        Some((node, "string"))
                    }
                    "integer_literal" | "float_literal" => Some((node, "number")),
                    "type_identifier" | "primitive_type" => Some((node, "type")),
                    "fn" | "let" | "mut" | "pub" | "use" | "mod" | "impl" | "trait" | "struct"
                    | "enum" | "match" | "if" | "else" | "for" | "while" | "loop" | "return"
                    | "async" | "await" | "move" | "const" | "static" | "where" | "self"
                    | "Self" | "crate" | "super" => Some((node, "keyword")),
                    "function_item" => node
                        .child_by_field_name("name")
                        .map(|name| (name, "function")),
                    _ => None,
                };
                if let Some((node, token)) = span {
                    spans.push(SyntaxSpan {
                        start: node.start_byte(),
                        end: node.end_byte(),
                        token,
                    });
                }
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    spans.sort_by_key(|span| (span.start, span.end));
                    spans.dedup_by_key(|span| (span.start, span.end));
                    return spans;
                }
            }
        }
    }

    #[must_use]
    pub fn outline(&self, source: &str) -> Vec<OutlineSymbol> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut cursor = tree.walk();
        let mut symbols = Vec::new();
        loop {
            let node = cursor.node();
            if matches!(node.kind(), "function_item" | "struct_item" | "enum_item")
                && let Some(name) = node.child_by_field_name("name")
                && let Ok(name) = name.utf8_text(source.as_bytes())
            {
                symbols.push(OutlineSymbol {
                    name: name.to_owned(),
                    kind: node.kind().to_owned(),
                    start: node.start_byte(),
                });
            }
            if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return symbols;
                }
            }
        }
    }

    #[must_use]
    pub fn folds(&self) -> Vec<(usize, usize)> {
        self.visible_nodes(0, usize::MAX)
            .into_iter()
            .filter(|(_, _, kind)| {
                matches!(
                    kind.as_str(),
                    "block" | "declaration_list" | "field_declaration_list"
                )
            })
            .map(|(start, end, _)| (start, end))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_rust_and_limits_nodes_to_viewport() {
        let mut doc = SyntaxDocument::rust(20).unwrap();
        assert!(doc.parse("fn main() { let x = 1; }"));
        assert!(
            doc.visible_nodes(0, 10)
                .iter()
                .any(|(_, _, kind)| kind == "function_item")
        );
    }
    #[test]
    fn huge_files_use_reduced_mode() {
        let mut doc = SyntaxDocument::rust(LARGE_FILE_BYTES + 1).unwrap();
        assert_eq!(doc.mode(), ParseMode::Reduced);
        assert!(!doc.parse("fn main() {}"));
    }
    #[test]
    fn exposes_highlights_folds_and_outline() {
        let source = "fn main() { let value: i32 = 42; }";
        let mut doc = SyntaxDocument::rust(source.len()).unwrap();
        doc.parse(source);
        assert_eq!(doc.outline(source)[0].name, "main");
        assert_ne!(doc.folds(), []);
        assert!(
            doc.highlight(0, source.len())
                .iter()
                .any(|span| span.token == "number")
        );
    }
}
