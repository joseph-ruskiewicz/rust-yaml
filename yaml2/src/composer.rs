//! The composer: events -> owned `Value` documents.

use std::collections::HashMap;

use crate::error::{Error, ErrorKind, Result, Span};
use crate::event::{Event, EventKind};
use crate::meta::ScalarStyle;
use crate::options::ParseOptions;
use crate::value::Value;

/// Composes a validated event stream into one `Value` per document.
pub(crate) fn compose(events: &[Event], options: &ParseOptions) -> Result<Vec<Value>> {
    let mut composer = Composer {
        events,
        pos: 0,
        options,
        anchors: HashMap::new(),
        alias_nodes: 0,
    };
    composer.compose_stream()
}

/// Cursor-based recursive composer over a borrowed event slice.
struct Composer<'a> {
    events: &'a [Event],
    pos: usize,
    options: &'a ParseOptions,
    /// Anchor name -> composed value, valid within the current document.
    anchors: HashMap<String, Value>,
    /// Running count of nodes materialized via alias expansion (billion-laughs guard).
    #[allow(dead_code)] // read once alias resolution lands (Task 4)
    alias_nodes: usize,
}

impl Composer<'_> {
    fn peek(&self) -> Option<&EventKind> {
        self.events.get(self.pos).map(|e| &e.kind)
    }

    /// Consumes and returns the next event, or a `Compose` error if the stream
    /// ends unexpectedly (the parser should never hand us a truncated stream).
    fn bump(&mut self) -> Result<Event> {
        let event = self
            .events
            .get(self.pos)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Compose, "unexpected end of event stream"))?;
        self.pos += 1;
        Ok(event)
    }

    fn error(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Compose, message).with_span(span)
    }

    fn compose_stream(&mut self) -> Result<Vec<Value>> {
        match self.bump()?.kind {
            EventKind::StreamStart => {}
            other => {
                return Err(Error::new(
                    ErrorKind::Compose,
                    format!("expected stream start, found {other:?}"),
                ))
            }
        }
        let mut documents = Vec::new();
        loop {
            match self.peek() {
                Some(EventKind::StreamEnd) => {
                    self.bump()?;
                    break;
                }
                Some(EventKind::DocumentStart) => documents.push(self.compose_document()?),
                None => break,
                Some(other) => {
                    let span = self.events[self.pos].span;
                    return Err(self.error(
                        span,
                        format!("expected document or stream end, found {other:?}"),
                    ));
                }
            }
        }
        Ok(documents)
    }

    fn compose_document(&mut self) -> Result<Value> {
        self.anchors.clear();
        self.bump()?; // DocumentStart
        let value = self.compose_node()?;
        match self.bump()?.kind {
            EventKind::DocumentEnd => {}
            other => {
                return Err(Error::new(
                    ErrorKind::Compose,
                    format!("expected document end, found {other:?}"),
                ))
            }
        }
        Ok(value)
    }

    fn compose_node(&mut self) -> Result<Value> {
        let event = self.bump()?;
        match event.kind {
            EventKind::Scalar {
                value,
                style,
                anchor,
                tag,
            } => {
                let composed = self.resolve_scalar(&value, style, tag.as_deref(), event.span)?;
                if let Some(name) = anchor {
                    self.anchors.insert(name, composed.clone());
                }
                Ok(composed)
            }
            EventKind::SequenceStart { anchor, tag } => self.compose_sequence(anchor, tag),
            other => Err(Error::new(
                ErrorKind::Compose,
                format!("unexpected event while composing a node: {other:?}"),
            )),
        }
    }

    fn compose_sequence(&mut self, anchor: Option<String>, _tag: Option<String>) -> Result<Value> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Some(EventKind::SequenceEnd) => {
                    self.bump()?;
                    break;
                }
                Some(_) => items.push(self.compose_node()?),
                None => {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "unterminated sequence in event stream",
                    ))
                }
            }
        }
        let value = Value::sequence(items);
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        Ok(value)
    }

    /// Resolves a scalar event into a typed `Value`. Untagged scalars use the
    /// configured schema; tagged scalars are handled in Task 6.
    fn resolve_scalar(
        &self,
        raw: &str,
        style: ScalarStyle,
        tag: Option<&str>,
        _span: Span,
    ) -> Result<Value> {
        let _ = tag; // tag handling added in Task 6
        Ok(Value::from_scalar(raw, style, self.options.schema))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Value {
        crate::api::parse(input).unwrap()
    }

    #[test]
    fn scalar_string_document() {
        assert_eq!(parse("hello\n").as_str(), Some("hello"));
    }

    #[test]
    fn scalar_int_document() {
        assert_eq!(parse("42\n").as_int(), Some(42));
    }

    #[test]
    fn scalar_bool_document() {
        assert_eq!(parse("true\n").as_bool(), Some(true));
    }

    #[test]
    fn empty_explicit_document_is_null() {
        assert!(parse("---\n").is_null());
    }

    #[test]
    fn empty_input_is_null() {
        assert!(parse("").is_null());
    }

    #[test]
    fn quoted_scalar_stays_string() {
        assert_eq!(parse("\"42\"\n").as_str(), Some("42"));
    }

    #[test]
    fn multiple_documents_compose_each() {
        let docs = crate::api::parse_documents("--- a\n--- b\n").unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].as_str(), Some("a"));
        assert_eq!(docs[1].as_str(), Some("b"));
    }

    #[test]
    fn parse_rejects_multiple_documents() {
        let err = crate::api::parse("--- a\n--- b\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn flow_sequence_of_scalars() {
        let v = parse("[1, 2, 3]\n");
        let items = v.as_sequence().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_int(), Some(1));
        assert_eq!(items[2].as_int(), Some(3));
    }

    #[test]
    fn block_sequence_of_scalars() {
        let v = parse("- a\n- b\n");
        let items = v.as_sequence().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("a"));
        assert_eq!(items[1].as_str(), Some("b"));
    }

    #[test]
    fn nested_sequence() {
        let v = parse("- - 1\n");
        let outer = v.as_sequence().unwrap();
        assert_eq!(outer.len(), 1);
        let inner = outer[0].as_sequence().unwrap();
        assert_eq!(inner[0].as_int(), Some(1));
    }

    #[test]
    fn empty_flow_sequence_is_empty() {
        assert_eq!(parse("[]\n").as_sequence().unwrap().len(), 0);
    }
}
