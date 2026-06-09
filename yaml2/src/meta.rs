//! Per-node formatting metadata, populated only when `preserve_formatting` is on.

use crate::error::Span;

/// How a scalar was written in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScalarStyle {
    #[default]
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

/// Comments attached to a node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    /// Full-line comments appearing immediately before the node.
    pub leading: Vec<String>,
    /// An inline comment on the same line as the node.
    pub trailing: Option<String>,
}

/// Optional formatting metadata for a node. Present only when round-tripping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub comments: Comments,
    pub blank_lines_before: usize,
    pub style: ScalarStyle,
    pub span: Option<Span>,
    pub anchor: Option<String>,
    pub tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_style_defaults_to_plain() {
        assert_eq!(ScalarStyle::default(), ScalarStyle::Plain);
    }

    #[test]
    fn meta_default_is_empty() {
        let m = Meta::default();
        assert!(m.comments.leading.is_empty());
        assert!(m.comments.trailing.is_none());
        assert_eq!(m.blank_lines_before, 0);
        assert_eq!(m.style, ScalarStyle::Plain);
        assert!(m.span.is_none());
        assert!(m.anchor.is_none());
        assert!(m.tag.is_none());
    }
}
