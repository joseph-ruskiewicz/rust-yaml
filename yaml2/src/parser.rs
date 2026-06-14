//! The event parser: tokens -> events (recursive descent, depth-bounded).

use crate::error::{Error, ErrorKind, Result, Span};
use crate::event::{Event, EventKind};
use crate::meta::ScalarStyle;
use crate::options::ParseOptions;
use crate::scanner::{tokenize, Token, TokenKind};

/// Parses `input` into a complete event vector.
pub fn parse_events(input: &str, options: &ParseOptions) -> Result<Vec<Event>> {
    let tokens = tokenize(input, options.limits)?;
    let mut parser = ParserState {
        tokens,
        pos: 0,
        events: Vec::new(),
        depth: 0,
        max_depth: options.limits.max_depth,
    };
    parser.parse_stream()?;
    Ok(parser.events)
}

/// A pull-based event parser over an input document.
pub struct Parser {
    events: std::vec::IntoIter<Event>,
}

impl Parser {
    /// Parses `input` eagerly; subsequent `next_event` calls drain the result.
    pub fn new(input: &str, options: &ParseOptions) -> Result<Self> {
        Ok(Self {
            events: parse_events(input, options)?.into_iter(),
        })
    }

    /// Returns the next event, or `None` once the stream is exhausted.
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.next()
    }
}

/// Internal recursive-descent state.
struct ParserState {
    tokens: Vec<Token>,
    pos: usize,
    events: Vec<Event>,
    depth: usize,
    max_depth: usize,
}

impl ParserState {
    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    /// The span of the current token (or the last token's span at end of input).
    fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|t| t.span)
            .unwrap_or_default()
    }

    /// Consumes and returns the current token. Callers must `peek` first.
    fn bump(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }

    fn emit(&mut self, kind: EventKind, span: Span) {
        self.events.push(Event::new(kind, span));
    }

    fn emit_empty_scalar(&mut self) {
        let span = self.span();
        self.emit(
            EventKind::Scalar {
                value: String::new(),
                style: ScalarStyle::Plain,
                anchor: None,
                tag: None,
            },
            span,
        );
    }

    fn error(&self, message: &str) -> Error {
        Error::new(ErrorKind::Parse, message).with_span(self.span())
    }

    fn parse_stream(&mut self) -> Result<()> {
        match self.peek() {
            Some(TokenKind::StreamStart) => {
                let span = self.span();
                self.bump();
                self.emit(EventKind::StreamStart, span);
            }
            _ => return Err(self.error("expected stream start")),
        }

        loop {
            let pos_before = self.pos;
            match self.peek() {
                None | Some(TokenKind::StreamEnd) => {
                    let span = self.span();
                    if self.peek().is_some() {
                        self.bump();
                    }
                    self.emit(EventKind::StreamEnd, span);
                    break;
                }
                Some(TokenKind::DocumentEnd) => {
                    self.bump();
                }
                Some(TokenKind::DocumentStart) => {
                    let span = self.span();
                    self.bump();
                    self.emit(EventKind::DocumentStart, span);
                    self.parse_document_content()?;
                    self.finish_document();
                }
                _ => {
                    let span = self.span();
                    self.emit(EventKind::DocumentStart, span);
                    self.parse_document_content()?;
                    self.finish_document();
                }
            }
            // No-progress guard: if an iteration consumed no token, the parser
            // would loop forever allocating events. Reject instead.
            if self.pos == pos_before {
                return Err(self.error("unexpected token at document level"));
            }
        }
        Ok(())
    }

    /// Parses a document's root node, or an empty node for an empty document.
    fn parse_document_content(&mut self) -> Result<()> {
        match self.peek() {
            None
            | Some(TokenKind::StreamEnd)
            | Some(TokenKind::DocumentStart)
            | Some(TokenKind::DocumentEnd) => {
                self.emit_empty_scalar();
                Ok(())
            }
            _ => self.parse_node(),
        }
    }

    /// Emits `DocumentEnd`, consuming an optional `...` token.
    fn finish_document(&mut self) {
        let span = self.span();
        if matches!(self.peek(), Some(TokenKind::DocumentEnd)) {
            self.bump();
        }
        self.emit(EventKind::DocumentEnd, span);
    }

    fn parse_node(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(
                Error::new(ErrorKind::LimitExceeded, "maximum nesting depth exceeded")
                    .with_span(self.span()),
            );
        }
        let result = self.parse_node_inner();
        self.depth -= 1;
        result
    }

    fn parse_node_inner(&mut self) -> Result<()> {
        // Node properties (anchor `&`, tag `!`) in any order.
        let mut anchor: Option<String> = None;
        let mut tag: Option<String> = None;
        loop {
            match self.peek() {
                Some(TokenKind::Anchor(_)) => {
                    if anchor.is_some() {
                        return Err(self.error("a node may have at most one anchor"));
                    }
                    if let TokenKind::Anchor(name) = self.bump().kind {
                        anchor = Some(name);
                    }
                }
                Some(TokenKind::Tag(_)) => {
                    if tag.is_some() {
                        return Err(self.error("a node may have at most one tag"));
                    }
                    if let TokenKind::Tag(t) = self.bump().kind {
                        tag = Some(t);
                    }
                }
                _ => break,
            }
        }

        let span = self.span();
        match self.peek() {
            Some(TokenKind::Scalar { .. }) => {
                let token = self.bump();
                if let TokenKind::Scalar { value, style } = token.kind {
                    self.emit(
                        EventKind::Scalar {
                            value,
                            style,
                            anchor,
                            tag,
                        },
                        token.span,
                    );
                }
                Ok(())
            }
            Some(TokenKind::Alias(_)) => {
                if anchor.is_some() || tag.is_some() {
                    return Err(self.error("an alias node cannot have an anchor or tag"));
                }
                let token = self.bump();
                if let TokenKind::Alias(name) = token.kind {
                    self.emit(EventKind::Alias(name), token.span);
                }
                Ok(())
            }
            Some(TokenKind::BlockSequenceStart) => self.parse_block_sequence(anchor, tag),
            Some(TokenKind::BlockMappingStart) => self.parse_block_mapping(anchor, tag),
            Some(TokenKind::BlockEntry) => self.parse_indentless_sequence(anchor, tag),
            Some(TokenKind::FlowSequenceStart) => self.parse_flow_sequence(anchor, tag),
            Some(TokenKind::FlowMappingStart) => self.parse_flow_mapping(anchor, tag),
            _ => {
                // Implicit empty (null) node — carries any collected properties.
                self.emit(
                    EventKind::Scalar {
                        value: String::new(),
                        style: ScalarStyle::Plain,
                        anchor,
                        tag,
                    },
                    span,
                );
                Ok(())
            }
        }
    }

    fn parse_block_sequence(&mut self, anchor: Option<String>, tag: Option<String>) -> Result<()> {
        let start = self.bump(); // BlockSequenceStart
        self.emit(EventKind::SequenceStart { anchor, tag }, start.span);
        loop {
            match self.peek() {
                Some(TokenKind::BlockEntry) => {
                    self.bump();
                    if matches!(
                        self.peek(),
                        Some(TokenKind::BlockEntry) | Some(TokenKind::BlockEnd)
                    ) {
                        self.emit_empty_scalar();
                    } else {
                        self.parse_node()?;
                    }
                }
                Some(TokenKind::BlockEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::SequenceEnd, end.span);
                    return Ok(());
                }
                _ => return Err(self.error("expected a block sequence entry or end")),
            }
        }
    }

    fn parse_block_mapping(&mut self, anchor: Option<String>, tag: Option<String>) -> Result<()> {
        let start = self.bump(); // BlockMappingStart
        self.emit(EventKind::MappingStart { anchor, tag }, start.span);
        loop {
            match self.peek() {
                Some(TokenKind::Key) => {
                    self.bump();
                    // Key node (empty if directly followed by Value/Key/BlockEnd).
                    if matches!(
                        self.peek(),
                        Some(TokenKind::Value) | Some(TokenKind::Key) | Some(TokenKind::BlockEnd)
                    ) {
                        self.emit_empty_scalar();
                    } else {
                        self.parse_node()?;
                    }
                    // Value node.
                    if matches!(self.peek(), Some(TokenKind::Value)) {
                        self.bump();
                        if matches!(
                            self.peek(),
                            Some(TokenKind::Key) | Some(TokenKind::BlockEnd)
                        ) {
                            self.emit_empty_scalar();
                        } else {
                            self.parse_node()?;
                        }
                    } else {
                        self.emit_empty_scalar();
                    }
                }
                Some(TokenKind::BlockEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::MappingEnd, end.span);
                    return Ok(());
                }
                _ => return Err(self.error("expected a block mapping key or end")),
            }
        }
    }

    /// Parses an indentless block sequence (bare `BlockEntry` tokens with no
    /// `BlockSequenceStart`/`BlockEnd`). It ends when the next token is not a
    /// `BlockEntry`.
    fn parse_indentless_sequence(
        &mut self,
        anchor: Option<String>,
        tag: Option<String>,
    ) -> Result<()> {
        let span = self.span();
        self.emit(EventKind::SequenceStart { anchor, tag }, span);
        loop {
            match self.peek() {
                Some(TokenKind::BlockEntry) => {
                    self.bump();
                    if matches!(
                        self.peek(),
                        Some(TokenKind::BlockEntry)
                            | Some(TokenKind::Key)
                            | Some(TokenKind::Value)
                            | Some(TokenKind::BlockEnd)
                    ) {
                        self.emit_empty_scalar();
                    } else {
                        self.parse_node()?;
                    }
                }
                _ => {
                    let end_span = self.span();
                    self.emit(EventKind::SequenceEnd, end_span);
                    return Ok(());
                }
            }
        }
    }

    /// Parses a flow sequence `[ ... ]`. Entries are comma-separated and may be
    /// single key/value pairs (`[a: b]`), which form implicit single-entry
    /// mappings.
    fn parse_flow_sequence(&mut self, anchor: Option<String>, tag: Option<String>) -> Result<()> {
        let start = self.bump(); // FlowSequenceStart
        self.emit(EventKind::SequenceStart { anchor, tag }, start.span);
        loop {
            if matches!(self.peek(), Some(TokenKind::FlowSequenceEnd)) {
                let end = self.bump();
                self.emit(EventKind::SequenceEnd, end.span);
                return Ok(());
            }
            // A ',' here means a leading or repeated separator with no entry.
            if matches!(self.peek(), Some(TokenKind::FlowEntry)) {
                return Err(self.error("unexpected ',' in flow sequence"));
            }
            self.parse_flow_sequence_entry()?;
            match self.peek() {
                Some(TokenKind::FlowEntry) => {
                    self.bump();
                }
                Some(TokenKind::FlowSequenceEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::SequenceEnd, end.span);
                    return Ok(());
                }
                _ => return Err(self.error("expected ',' or ']' in flow sequence")),
            }
        }
    }

    /// Parses one flow sequence entry: either a node, or a single key/value
    /// pair wrapped in an implicit single-entry mapping.
    fn parse_flow_sequence_entry(&mut self) -> Result<()> {
        let mark = self.events.len();
        let key_span = self.span();

        // `[: v]` — an entry that begins with `:` is a pair with an empty key.
        if matches!(self.peek(), Some(TokenKind::Value)) {
            self.emit(
                EventKind::MappingStart {
                    anchor: None,
                    tag: None,
                },
                key_span,
            );
            self.emit_empty_scalar();
            self.parse_flow_pair_value()?;
            return Ok(());
        }

        // Parse the entry node — the candidate key of a single pair.
        self.parse_node()?;

        // A trailing `:` promotes this entry to a single-pair mapping.
        if matches!(self.peek(), Some(TokenKind::Value)) {
            self.events.insert(
                mark,
                Event::new(
                    EventKind::MappingStart {
                        anchor: None,
                        tag: None,
                    },
                    key_span,
                ),
            );
            self.parse_flow_pair_value()?;
        }
        Ok(())
    }

    /// Consumes the `:` and parses the value of a flow single-pair mapping,
    /// then emits `MappingEnd`. The key (and `MappingStart`) are already emitted.
    fn parse_flow_pair_value(&mut self) -> Result<()> {
        self.bump(); // Value
        if self.at_flow_value_end() {
            self.emit_empty_scalar();
        } else {
            self.parse_node()?;
        }
        let end_span = self.span();
        self.emit(EventKind::MappingEnd, end_span);
        Ok(())
    }

    /// True when the current token terminates a flow value (separator or close).
    fn at_flow_value_end(&self) -> bool {
        matches!(
            self.peek(),
            None | Some(TokenKind::FlowEntry)
                | Some(TokenKind::FlowSequenceEnd)
                | Some(TokenKind::FlowMappingEnd)
        )
    }

    /// True when the current token terminates a flow key (a `:` value indicator
    /// or any flow-value terminator).
    fn at_flow_key_end(&self) -> bool {
        matches!(self.peek(), Some(TokenKind::Value)) || self.at_flow_value_end()
    }

    /// Parses a flow mapping `{ ... }`. Entries are comma-separated key/value
    /// pairs; keys may be explicit (`? k`) or implicit, and either side may be
    /// empty (`{a}`, `{:b}`, `{a:}`).
    fn parse_flow_mapping(&mut self, anchor: Option<String>, tag: Option<String>) -> Result<()> {
        let start = self.bump(); // FlowMappingStart
        self.emit(EventKind::MappingStart { anchor, tag }, start.span);
        loop {
            if matches!(self.peek(), Some(TokenKind::FlowMappingEnd)) {
                let end = self.bump();
                self.emit(EventKind::MappingEnd, end.span);
                return Ok(());
            }
            // A ',' here means a leading or repeated separator with no entry.
            if matches!(self.peek(), Some(TokenKind::FlowEntry)) {
                return Err(self.error("unexpected ',' in flow mapping"));
            }
            self.parse_flow_mapping_entry()?;
            match self.peek() {
                Some(TokenKind::FlowEntry) => {
                    self.bump();
                }
                Some(TokenKind::FlowMappingEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::MappingEnd, end.span);
                    return Ok(());
                }
                _ => return Err(self.error("expected ',' or '}' in flow mapping")),
            }
        }
    }

    /// Parses one flow mapping key/value pair, emitting a key node followed by a
    /// value node (either may be an implicit empty scalar).
    fn parse_flow_mapping_entry(&mut self) -> Result<()> {
        // Key.
        match self.peek() {
            Some(TokenKind::Key) => {
                self.bump(); // `?`
                if self.at_flow_key_end() {
                    self.emit_empty_scalar();
                } else {
                    self.parse_node()?;
                }
            }
            Some(TokenKind::Value) => {
                self.emit_empty_scalar(); // empty key; `:` consumed below
            }
            _ => self.parse_node()?,
        }
        // Value.
        if matches!(self.peek(), Some(TokenKind::Value)) {
            self.bump();
            if self.at_flow_value_end() {
                self.emit_empty_scalar();
            } else {
                self.parse_node()?;
            }
        } else {
            self.emit_empty_scalar();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &str) -> Vec<EventKind> {
        parse_events(input, &ParseOptions::default())
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn empty_input_is_stream_start_then_end() {
        assert_eq!(
            kinds(""),
            vec![EventKind::StreamStart, EventKind::StreamEnd]
        );
    }

    #[test]
    fn comment_only_is_stream_start_then_end() {
        assert_eq!(
            kinds("# just a comment\n"),
            vec![EventKind::StreamStart, EventKind::StreamEnd]
        );
    }

    #[test]
    fn parser_pull_api_drains_events() {
        let mut p = Parser::new("", &ParseOptions::default()).unwrap();
        assert_eq!(p.next_event().map(|e| e.kind), Some(EventKind::StreamStart));
        assert_eq!(p.next_event().map(|e| e.kind), Some(EventKind::StreamEnd));
        assert_eq!(p.next_event(), None);
    }

    #[test]
    fn bare_scalar_document() {
        assert_eq!(
            kinds("hello\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn explicit_document_marker() {
        assert_eq!(
            kinds("--- hello\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn two_documents() {
        assert_eq!(
            kinds("--- a\n--- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::DocumentEnd,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn alias_node() {
        assert_eq!(
            kinds("*x\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Alias("x".to_string()),
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_sequence() {
        assert_eq!(
            kinds("- a\n- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_block_sequence() {
        assert_eq!(
            kinds("- - a\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_mapping() {
        assert_eq!(
            kinds("a: 1\nb: 2\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_block_mapping() {
        assert_eq!(
            kinds("outer:\n  inner: v\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "outer".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "inner".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn mapping_in_sequence_entry() {
        assert_eq!(
            kinds("- k: v\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "k".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn indentless_sequence_value() {
        assert_eq!(
            kinds("items:\n- a\n- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "items".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_sequence() {
        assert_eq!(
            kinds("[a, b]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_sequence_single_pair_mapping() {
        assert_eq!(
            kinds("[a: b]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_flow_sequence() {
        assert_eq!(
            kinds("[]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_flow_sequence() {
        assert_eq!(
            kinds("[[a], b]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_sequence_trailing_comma() {
        assert_eq!(
            kinds("[a, b,]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_mapping() {
        assert_eq!(
            kinds("{a: b, c: d}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "c".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "d".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_flow_mapping() {
        assert_eq!(
            kinds("{}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_mapping_key_only_has_empty_value() {
        assert_eq!(
            kinds("{a}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: String::new(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_mapping_empty_key() {
        assert_eq!(
            kinds("{: b}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: String::new(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_mapping_explicit_key() {
        assert_eq!(
            kinds("{? a : b}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_flow_collections() {
        assert_eq!(
            kinds("{k: [1, 2]}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "k".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchor_on_flow_sequence() {
        assert_eq!(
            kinds("&a [1]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: Some("a".to_string()),
                    tag: None
                },
                EventKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn tag_on_flow_mapping() {
        assert_eq!(
            kinds("!!map {a: b}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: Some("tag:yaml.org,2002:map".to_string())
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchor_on_block_sequence() {
        assert_eq!(
            kinds("&items\n- a\n- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: Some("items".to_string()),
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchor_and_tag_on_block_mapping() {
        assert_eq!(
            kinds("&m !!map\na: 1\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: Some("m".to_string()),
                    tag: Some("tag:yaml.org,2002:map".to_string())
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn mapping_with_empty_value() {
        assert_eq!(
            kinds("a:\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn sequence_with_empty_entry() {
        assert_eq!(
            kinds("-\n- a\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_document() {
        assert_eq!(
            kinds("---\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn deeply_nested_flow_exceeds_max_depth() {
        let opts = ParseOptions {
            limits: crate::options::Limits {
                max_depth: 16,
                ..Default::default()
            },
            ..Default::default()
        };
        let input = "[".repeat(50) + &"]".repeat(50);
        let err = parse_events(&input, &opts).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::LimitExceeded);
    }

    #[test]
    fn nesting_within_limit_is_ok() {
        let opts = ParseOptions {
            limits: crate::options::Limits {
                max_depth: 16,
                ..Default::default()
            },
            ..Default::default()
        };
        // 5 levels of flow sequence nesting, well within the limit.
        assert!(parse_events("[[[[[x]]]]]", &opts).is_ok());
    }

    #[test]
    fn malformed_double_colon_is_rejected() {
        // `a: b: c` yields a token stream with a mapping start followed by a
        // Value with no Key — a Parse error, not garbage events.
        let err = parse_events("a: b: c\n", &ParseOptions::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Parse);
    }

    #[test]
    fn realistic_document_events() {
        let input = "name: Ada\njobs:\n  - lang: rust\n    years: 3\n  - lang: yaml\n";
        assert_eq!(
            kinds(input),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "name".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "Ada".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "jobs".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::SequenceStart {
                    anchor: None,
                    tag: None
                },
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "lang".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "rust".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "years".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "3".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "lang".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "yaml".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::SequenceEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_scalar_value_event() {
        assert_eq!(
            kinds("text: |\n  a\n  b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart {
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "text".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None
                },
                EventKind::Scalar {
                    value: "a\nb\n".to_string(),
                    style: ScalarStyle::Literal,
                    anchor: None,
                    tag: None
                },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn unexpected_document_level_token_errors_without_hanging() {
        let err = parse_events("]", &ParseOptions::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Parse);
    }

    #[test]
    fn flow_sequence_leading_comma_is_error() {
        let err = parse_events("[, a]", &ParseOptions::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Parse);
    }

    #[test]
    fn flow_sequence_repeated_comma_is_error() {
        let err = parse_events("[a, , b]", &ParseOptions::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Parse);
    }

    #[test]
    fn flow_sequence_trailing_comma_is_ok() {
        // A single trailing comma is valid; only leading/repeated commas error.
        assert!(parse_events("[a, b,]", &ParseOptions::default()).is_ok());
    }

    #[test]
    fn flow_mapping_leading_comma_is_error() {
        let err = parse_events("{, a: b}", &ParseOptions::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Parse);
    }
}
