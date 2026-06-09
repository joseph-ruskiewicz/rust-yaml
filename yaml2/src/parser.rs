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
    #[allow(dead_code)] // used in Task P5.11 (depth limit)
    depth: usize,
    #[allow(dead_code)] // used in Task P5.11 (depth limit)
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

    /// Parses a single node. Temporary stub (emits empty scalar); replaced in
    /// Task P5.3.
    fn parse_node(&mut self) -> Result<()> {
        self.emit_empty_scalar();
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
        assert_eq!(kinds(""), vec![EventKind::StreamStart, EventKind::StreamEnd]);
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
}
