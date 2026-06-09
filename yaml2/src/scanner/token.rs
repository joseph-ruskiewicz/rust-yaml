//! The token type produced by the scanner.

use crate::error::Span;
use crate::meta::ScalarStyle;

/// A lexical token with its source span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub(crate) fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The lexical categories the scanner emits.
///
/// Block-structure tokens (block mapping/sequence start/end, block entry) and
/// directives are added in Plan 3; this set covers the flow subset.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    StreamStart,
    StreamEnd,
    /// `---`
    DocumentStart,
    /// `...`
    DocumentEnd,
    /// `[`
    FlowSequenceStart,
    /// `]`
    FlowSequenceEnd,
    /// `{`
    FlowMappingStart,
    /// `}`
    FlowMappingEnd,
    /// `,`
    FlowEntry,
    /// `?` (explicit key indicator)
    Key,
    /// `:` (value indicator)
    Value,
    /// `&name`
    Anchor(String),
    /// `*name`
    Alias(String),
    /// `!tag` (raw text including the leading `!`; full resolution is later)
    Tag(String),
    /// A scalar value with the style it was written in.
    Scalar {
        value: String,
        style: ScalarStyle,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Position;

    #[test]
    fn token_holds_kind_and_span() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(1, 1, 2));
        let t = Token::new(TokenKind::FlowEntry, span);
        assert_eq!(t.kind, TokenKind::FlowEntry);
        assert_eq!(t.span, span);
    }

    #[test]
    fn scalar_kind_carries_value_and_style() {
        let kind = TokenKind::Scalar {
            value: "hi".to_string(),
            style: ScalarStyle::Plain,
        };
        assert_eq!(
            kind,
            TokenKind::Scalar {
                value: "hi".to_string(),
                style: ScalarStyle::Plain
            }
        );
    }
}
