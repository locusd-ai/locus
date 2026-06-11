//! Filter expression parser for Locus query DSL.
//!
//! Grammar (precedence, lowest to highest):
//! ```text
//! expr    := or_expr
//! or_expr := and_expr ("OR" and_expr)*
//! and_expr:= not_expr ("AND" not_expr)*
//! not_expr:= "NOT" not_expr | primary
//! primary := "(" expr ")" | key_term
//! key_term:= NAMESPACE ":" VALUE
//!
//! NAMESPACE := [a-zA-Z][a-zA-Z0-9_-]*
//! VALUE     := [a-zA-Z0-9/_\-.:]+ (at least one char)
//! ```
//!
//! Bare terms without a colon are rejected with a helpful error listing known namespaces.
//! AND/OR/NOT are case-insensitive keywords; they cannot be used as namespace names.
//!
//! # Examples
//! ```ignore
//! tag:work AND (type:task OR type:note) AND NOT folder:archive
//! source:code AND lang:rust
//! folder:crates/locus-query AND NOT kind:test
//! ```

use locus_core::query::Filter;

// ── Error type ────────────────────────────────────────────────────

/// Errors produced by the filter expression parser.
#[derive(Debug, thiserror::Error)]
pub enum FilterParseError {
    #[error("unexpected end of input at position {pos}: {msg}")]
    UnexpectedEof { pos: usize, msg: String },

    #[error("unexpected token '{token}' at position {pos}: {msg}")]
    UnexpectedToken {
        token: String,
        pos: usize,
        msg: String,
    },

    #[error("bare term '{term}' at position {pos}: terms must be namespaced (e.g. tag:work, folder:path, type:task, source:code, lang:rust, kind:function)")]
    BareTerm { term: String, pos: usize },

    #[error("empty parentheses at position {pos}")]
    EmptyParens { pos: usize },

    #[error("missing closing ')' for '(' at position {pos}")]
    UnclosedParen { pos: usize },

    #[error("empty value for key '{namespace}' at position {pos}")]
    EmptyValue { namespace: String, pos: usize },
}

// ── Token types ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    And,
    Or,
    Not,
    LParen,
    RParen,
    Key(String),  // "namespace:value"
}

// ── Lexer ─────────────────────────────────────────────────────────

struct Lexer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len()
            && self.input.as_bytes()[self.pos].is_ascii_whitespace()
        {
            self.pos += 1;
        }
    }

    /// Read an identifier: alphanumeric, `-`, `_`. Used for namespace/keywords.
    fn read_ident(&mut self) -> &'a str {
        let start = self.pos;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        &self.input[start..self.pos]
    }

    /// Read a value after the colon: alphanumeric plus `/`, `-`, `_`, `.`, `:`.
    fn read_value(&mut self) -> &'a str {
        let start = self.pos;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b':') {
                self.pos += 1;
            } else {
                break;
            }
        }
        &self.input[start..self.pos]
    }

    fn tokenize(mut self) -> Result<Vec<(usize, Token)>, FilterParseError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            if self.pos >= self.input.len() {
                break;
            }
            let tok_pos = self.pos;
            let b = self.input.as_bytes()[self.pos];
            match b {
                b'(' => {
                    tokens.push((tok_pos, Token::LParen));
                    self.pos += 1;
                }
                b')' => {
                    tokens.push((tok_pos, Token::RParen));
                    self.pos += 1;
                }
                _ if b.is_ascii_alphabetic() => {
                    let ident = self.read_ident();
                    // After the ident, check if it's a key (followed by ':' + value),
                    // a keyword, or a bare term (error).
                    self.skip_whitespace();

                    // Peek: is the NEXT char a colon?
                    if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == b':' {
                        // It's a key term: namespace:value
                        self.pos += 1; // consume ':'
                        let val_pos = self.pos;
                        let value = self.read_value();
                        if value.is_empty() {
                            return Err(FilterParseError::EmptyValue {
                                namespace: ident.to_string(),
                                pos: val_pos,
                            });
                        }
                        let key = format!("{ident}:{value}");
                        tokens.push((tok_pos, Token::Key(key)));
                    } else {
                        // Keyword or bare term
                        match ident.to_uppercase().as_str() {
                            "AND" => tokens.push((tok_pos, Token::And)),
                            "OR" => tokens.push((tok_pos, Token::Or)),
                            "NOT" => tokens.push((tok_pos, Token::Not)),
                            _ => {
                                return Err(FilterParseError::BareTerm {
                                    term: ident.to_string(),
                                    pos: tok_pos,
                                });
                            }
                        }
                    }
                }
                other => {
                    let ch = other as char;
                    return Err(FilterParseError::UnexpectedToken {
                        token: ch.to_string(),
                        pos: tok_pos,
                        msg: "unexpected character".to_string(),
                    });
                }
            }
        }
        Ok(tokens)
    }
}

// ── Recursive-descent parser ──────────────────────────────────────

struct Parser {
    tokens: Vec<(usize, Token)>,
    cursor: usize,
}

impl Parser {
    fn new(tokens: Vec<(usize, Token)>) -> Self {
        Self { tokens, cursor: 0 }
    }

    fn peek(&self) -> Option<&(usize, Token)> {
        self.tokens.get(self.cursor)
    }

    fn peek_pos(&self) -> usize {
        self.tokens
            .get(self.cursor)
            .map(|(p, _)| *p)
            .unwrap_or(usize::MAX)
    }

    fn advance(&mut self) -> Option<&(usize, Token)> {
        let tok = self.tokens.get(self.cursor);
        self.cursor += 1;
        tok
    }

    fn is_at_end(&self) -> bool {
        self.cursor >= self.tokens.len()
    }

    // expr = or_expr
    fn parse_expr(&mut self) -> Result<Filter, FilterParseError> {
        self.parse_or_expr()
    }

    // or_expr = and_expr ("OR" and_expr)*
    fn parse_or_expr(&mut self) -> Result<Filter, FilterParseError> {
        let mut lhs = self.parse_and_expr()?;

        while let Some((_, Token::Or)) = self.peek() {
            self.advance(); // consume OR
            let rhs = self.parse_and_expr()?;
            // Flatten nested OR
            lhs = match lhs {
                Filter::Or(mut children) => {
                    children.push(rhs);
                    Filter::Or(children)
                }
                other => Filter::Or(vec![other, rhs]),
            };
        }

        Ok(lhs)
    }

    // and_expr = not_expr ("AND" not_expr)*
    fn parse_and_expr(&mut self) -> Result<Filter, FilterParseError> {
        let mut lhs = self.parse_not_expr()?;

        while let Some((_, Token::And)) = self.peek() {
            self.advance(); // consume AND
            let rhs = self.parse_not_expr()?;
            // Flatten nested AND
            lhs = match lhs {
                Filter::And(mut children) => {
                    children.push(rhs);
                    Filter::And(children)
                }
                other => Filter::And(vec![other, rhs]),
            };
        }

        Ok(lhs)
    }

    // not_expr = "NOT" not_expr | primary
    fn parse_not_expr(&mut self) -> Result<Filter, FilterParseError> {
        if let Some((_, Token::Not)) = self.peek() {
            self.advance(); // consume NOT
            let inner = self.parse_not_expr()?;
            return Ok(Filter::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    // primary = "(" expr ")" | key_term
    fn parse_primary(&mut self) -> Result<Filter, FilterParseError> {
        match self.peek() {
            None => Err(FilterParseError::UnexpectedEof {
                pos: self.peek_pos(),
                msg: "expected a key term or '(' expression".to_string(),
            }),
            Some((_, Token::LParen)) => {
                let open_pos = self.peek_pos();
                self.advance(); // consume '('

                if let Some((_, Token::RParen)) = self.peek() {
                    return Err(FilterParseError::EmptyParens { pos: open_pos });
                }

                let inner = self.parse_expr()?;

                match self.advance() {
                    Some((_, Token::RParen)) => Ok(inner),
                    _ => Err(FilterParseError::UnclosedParen { pos: open_pos }),
                }
            }
            Some((_, Token::Key(k))) => {
                let key = k.clone();
                self.advance();
                Ok(Filter::Key(key))
            }
            Some((pos, tok)) => {
                let pos = *pos;
                let tok_str = match tok {
                    Token::And => "AND".to_string(),
                    Token::Or => "OR".to_string(),
                    Token::Not => "NOT".to_string(),
                    Token::RParen => ")".to_string(),
                    Token::LParen => "(".to_string(),
                    Token::Key(k) => k.clone(),
                };
                Err(FilterParseError::UnexpectedToken {
                    token: tok_str,
                    pos,
                    msg: "expected a key term (namespace:value) or '(' expression".to_string(),
                })
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────

/// Parse a filter expression string into a `Filter` AST.
///
/// # Grammar
/// - Terms: `namespace:value` (e.g. `tag:work`, `folder:crates/locus-query`)
/// - Operators: `AND`, `OR`, `NOT` (case-insensitive)
/// - Grouping: `(...)` for explicit precedence
/// - Precedence: NOT > AND > OR
///
/// # Errors
/// Returns a descriptive error with position information on any parse failure.
/// Bare terms without a colon are rejected with suggestions for valid namespaces.
pub fn parse_filter(input: &str) -> Result<Filter, FilterParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(FilterParseError::UnexpectedEof {
            pos: 0,
            msg: "filter expression is empty".to_string(),
        });
    }

    let tokens = Lexer::new(trimmed).tokenize()?;
    if tokens.is_empty() {
        return Err(FilterParseError::UnexpectedEof {
            pos: 0,
            msg: "filter expression produced no tokens".to_string(),
        });
    }

    let mut parser = Parser::new(tokens);
    let filter = parser.parse_expr()?;

    // Ensure all tokens were consumed (no trailing garbage)
    if !parser.is_at_end() {
        let (pos, tok) = &parser.tokens[parser.cursor];
        let tok_str = match tok {
            Token::And => "AND".to_string(),
            Token::Or => "OR".to_string(),
            Token::Not => "NOT".to_string(),
            Token::RParen => ")".to_string(),
            Token::LParen => "(".to_string(),
            Token::Key(k) => k.clone(),
        };
        return Err(FilterParseError::UnexpectedToken {
            token: tok_str,
            pos: *pos,
            msg: "unexpected trailing token after complete expression".to_string(),
        });
    }

    Ok(filter)
}

/// Build a match-all filter covering every `source:*` key in the bitmap store.
///
/// Returns `Filter::Or(vec![Filter::Key("source:X"), ...])` over every key with
/// the `source:` prefix. If the store has no source keys, returns `Filter::Or(vec![])`,
/// which `resolve_filter` evaluates to an empty bitmap — correct for an empty index.
pub fn match_all_filter(store: &dyn locus_core::bitmap::BitmapStore) -> Filter {
    let keys = store.list_keys(Some("source:")).unwrap_or_default();
    Filter::Or(keys.into_iter().map(Filter::Key).collect())
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to assert successful parse produces a given Filter structure
    fn parses(input: &str) -> Filter {
        parse_filter(input).expect("expected successful parse")
    }

    fn parse_err(input: &str) -> FilterParseError {
        parse_filter(input).expect_err("expected parse error")
    }

    // ── Basic key terms ───────────────────────────────────────────

    #[test]
    fn single_key_term() {
        let f = parses("tag:work");
        assert!(matches!(f, Filter::Key(k) if k == "tag:work"));
    }

    #[test]
    fn key_with_slash_in_value() {
        let f = parses("folder:crates/locus-query");
        assert!(matches!(f, Filter::Key(k) if k == "folder:crates/locus-query"));
    }

    #[test]
    fn key_with_dot_in_value() {
        let f = parses("source:code.ts");
        assert!(matches!(f, Filter::Key(k) if k == "source:code.ts"));
    }

    #[test]
    fn key_with_nested_colon_in_value() {
        // e.g. lang:rust:stable
        let f = parses("lang:rust:stable");
        assert!(matches!(f, Filter::Key(k) if k == "lang:rust:stable"));
    }

    #[test]
    fn key_with_dash_in_value() {
        let f = parses("kind:fn-async");
        assert!(matches!(f, Filter::Key(k) if k == "kind:fn-async"));
    }

    // ── AND ───────────────────────────────────────────────────────

    #[test]
    fn simple_and() {
        let f = parses("tag:work AND type:task");
        match f {
            Filter::And(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], Filter::Key(k) if k == "tag:work"));
                assert!(matches!(&children[1], Filter::Key(k) if k == "type:task"));
            }
            other => panic!("expected AND, got {:?}", other),
        }
    }

    #[test]
    fn and_three_terms_flattens() {
        let f = parses("tag:work AND type:task AND folder:archive");
        match f {
            Filter::And(children) => assert_eq!(children.len(), 3),
            other => panic!("expected flat AND(3), got {:?}", other),
        }
    }

    #[test]
    fn and_case_insensitive() {
        let f = parses("tag:work and type:task");
        assert!(matches!(f, Filter::And(_)));
        let f2 = parses("tag:work And type:task");
        assert!(matches!(f2, Filter::And(_)));
    }

    // ── OR ────────────────────────────────────────────────────────

    #[test]
    fn simple_or() {
        let f = parses("type:task OR type:note");
        match f {
            Filter::Or(children) => {
                assert_eq!(children.len(), 2);
            }
            other => panic!("expected OR, got {:?}", other),
        }
    }

    #[test]
    fn or_three_terms_flattens() {
        let f = parses("type:task OR type:note OR type:moc");
        match f {
            Filter::Or(children) => assert_eq!(children.len(), 3),
            other => panic!("expected flat OR(3), got {:?}", other),
        }
    }

    // ── NOT ───────────────────────────────────────────────────────

    #[test]
    fn not_term() {
        let f = parses("NOT folder:archive");
        match f {
            Filter::Not(inner) => {
                assert!(matches!(*inner, Filter::Key(k) if k == "folder:archive"));
            }
            other => panic!("expected NOT, got {:?}", other),
        }
    }

    #[test]
    fn double_not() {
        let f = parses("NOT NOT tag:work");
        assert!(matches!(f, Filter::Not(inner) if matches!(*inner, Filter::Not(_))));
    }

    // ── Precedence ────────────────────────────────────────────────

    #[test]
    fn and_binds_tighter_than_or() {
        // "a OR b AND c" should be "a OR (b AND c)"
        let f = parses("type:task OR tag:work AND folder:projects");
        match f {
            Filter::Or(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], Filter::Key(k) if k == "type:task"));
                assert!(matches!(&children[1], Filter::And(_)));
            }
            other => panic!("expected OR at top, got {:?}", other),
        }
    }

    #[test]
    fn not_binds_tighter_than_and() {
        // "NOT a AND b" should be "(NOT a) AND b"
        let f = parses("NOT folder:archive AND tag:work");
        match f {
            Filter::And(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], Filter::Not(_)));
                assert!(matches!(&children[1], Filter::Key(k) if k == "tag:work"));
            }
            other => panic!("expected AND at top, got {:?}", other),
        }
    }

    // ── Parentheses ───────────────────────────────────────────────

    #[test]
    fn parens_override_precedence() {
        // "(a OR b) AND c" — parens make OR the inner expression
        let f = parses("(type:task OR type:note) AND tag:work");
        match f {
            Filter::And(children) => {
                assert_eq!(children.len(), 2);
                assert!(matches!(&children[0], Filter::Or(_)));
                assert!(matches!(&children[1], Filter::Key(k) if k == "tag:work"));
            }
            other => panic!("expected AND at top, got {:?}", other),
        }
    }

    #[test]
    fn nested_parens() {
        let f = parses("tag:work AND (type:task OR (type:note AND NOT folder:archive))");
        assert!(matches!(f, Filter::And(_)));
    }

    #[test]
    fn complex_expression() {
        // Full expression from the task description
        let f = parses("tag:work AND (type:task OR type:note) AND NOT folder:archive");
        match f {
            Filter::And(children) => assert_eq!(children.len(), 3),
            other => panic!("expected flat AND(3), got {:?}", other),
        }
    }

    // ── Whitespace handling ───────────────────────────────────────

    #[test]
    fn extra_whitespace() {
        let f = parses("  tag:work   AND   type:task  ");
        assert!(matches!(f, Filter::And(_)));
    }

    // ── Error cases ───────────────────────────────────────────────

    #[test]
    fn bare_term_is_error() {
        let e = parse_err("work");
        assert!(matches!(e, FilterParseError::BareTerm { .. }));
        let msg = e.to_string();
        assert!(msg.contains("tag:"));
    }

    #[test]
    fn empty_input_is_error() {
        let e = parse_err("");
        assert!(matches!(e, FilterParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn whitespace_only_is_error() {
        let e = parse_err("   ");
        assert!(matches!(e, FilterParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn empty_parens_is_error() {
        let e = parse_err("()");
        assert!(matches!(e, FilterParseError::EmptyParens { .. }));
    }

    #[test]
    fn unclosed_paren_is_error() {
        let e = parse_err("(tag:work AND type:task");
        assert!(matches!(e, FilterParseError::UnclosedParen { .. }));
    }

    #[test]
    fn trailing_garbage_is_error() {
        let e = parse_err("tag:work AND type:task OR");
        assert!(matches!(e, FilterParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn empty_value_is_error() {
        // "tag:" with nothing after colon — lexer catches this
        let e = parse_err("tag:");
        assert!(matches!(e, FilterParseError::EmptyValue { .. }));
    }

    #[test]
    fn double_and_is_error() {
        let e = parse_err("tag:work AND AND type:task");
        // AND AND — the second AND is parsed as a primary, which is unexpected
        assert!(matches!(
            e,
            FilterParseError::UnexpectedToken { .. } | FilterParseError::UnexpectedEof { .. }
        ));
    }

    #[test]
    fn dangling_or_is_error() {
        let e = parse_err("tag:work OR");
        assert!(matches!(e, FilterParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn dangling_and_is_error() {
        let e = parse_err("tag:work AND");
        assert!(matches!(e, FilterParseError::UnexpectedEof { .. }));
    }

    // ── match_all_filter ──────────────────────────────────────────

    #[test]
    fn match_all_empty_store_returns_empty_or() {
        use locus_bitmap::memory::InMemoryBitmapStore;
        let store = InMemoryBitmapStore::new();
        let f = match_all_filter(&store);
        assert!(matches!(f, Filter::Or(ref v) if v.is_empty()));
    }

    #[test]
    fn match_all_with_sources_returns_or_of_source_keys() {
        use locus_bitmap::memory::InMemoryBitmapStore;
        use locus_core::bitmap::BitmapStore;

        let mut store = InMemoryBitmapStore::new();
        store.insert_id("source:obsidian", 1).unwrap();
        store.insert_id("source:code", 2).unwrap();
        store.insert_id("tag:work", 1).unwrap(); // should NOT be included

        let f = match_all_filter(&store);
        match f {
            Filter::Or(children) => {
                assert_eq!(children.len(), 2);
                // All children should be source: keys
                for child in &children {
                    assert!(
                        matches!(child, Filter::Key(k) if k.starts_with("source:")),
                        "expected source: key, got {:?}",
                        child
                    );
                }
            }
            other => panic!("expected OR, got {:?}", other),
        }
    }
}
