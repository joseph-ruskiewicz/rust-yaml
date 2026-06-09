//! The composer: events -> owned `Value` documents.

use std::collections::HashMap;

use crate::error::{Error, ErrorKind, Result, Span};
use crate::event::{Event, EventKind};
use crate::meta::ScalarStyle;
use crate::options::ParseOptions;
use crate::value::{Mapping, Value, ValueData};

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
            EventKind::MappingStart { anchor, tag } => self.compose_mapping(anchor, tag),
            EventKind::Alias(name) => self.resolve_alias(&name, event.span),
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

    fn compose_mapping(&mut self, anchor: Option<String>, _tag: Option<String>) -> Result<Value> {
        let mut map = Mapping::new();
        loop {
            match self.peek() {
                Some(EventKind::MappingEnd) => {
                    self.bump()?;
                    break;
                }
                Some(_) => {
                    let k = self.compose_node()?;
                    let val = self.compose_node()?;
                    map.insert(k, val);
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "unterminated mapping in event stream",
                    ))
                }
            }
        }
        let value = Value::mapping(map);
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        Ok(value)
    }

    fn resolve_alias(&mut self, name: &str, span: Span) -> Result<Value> {
        let value = self
            .anchors
            .get(name)
            .cloned()
            .ok_or_else(|| self.error(span, format!("unknown anchor '{name}'")))?;
        self.alias_nodes += count_nodes(&value);
        if self.alias_nodes > self.options.limits.max_aliases {
            return Err(
                Error::new(ErrorKind::LimitExceeded, "alias expansion limit exceeded")
                    .with_span(span),
            );
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

/// Counts the nodes in a value tree (the node itself plus all descendants).
/// Used to budget alias expansion against `Limits::max_aliases`.
fn count_nodes(value: &Value) -> usize {
    1 + match value.data() {
        ValueData::Sequence(items) => items.iter().map(count_nodes).sum(),
        ValueData::Mapping(map) => map
            .iter()
            .map(|(k, v)| count_nodes(k) + count_nodes(v))
            .sum(),
        _ => 0,
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

    fn key(s: &str) -> Value {
        Value::string(s)
    }

    #[test]
    fn flow_mapping_of_scalars() {
        let v = parse("{a: 1, b: 2}\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(m.get(&key("b")).unwrap().as_int(), Some(2));
    }

    #[test]
    fn block_mapping_of_scalars() {
        let v = parse("a: 1\nb: 2\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(m.get(&key("b")).unwrap().as_int(), Some(2));
    }

    #[test]
    fn nested_mapping() {
        let v = parse("outer:\n  inner: 7\n");
        let outer = v.as_mapping().unwrap();
        let inner = outer.get(&key("outer")).unwrap().as_mapping().unwrap();
        assert_eq!(inner.get(&key("inner")).unwrap().as_int(), Some(7));
    }

    #[test]
    fn mapping_empty_value_is_null() {
        let v = parse("a:\n");
        let m = v.as_mapping().unwrap();
        assert!(m.get(&key("a")).unwrap().is_null());
    }

    #[test]
    fn duplicate_keys_last_wins() {
        let v = parse("{a: 1, a: 2}\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&key("a")).unwrap().as_int(), Some(2));
    }

    #[test]
    fn alias_clones_scalar_anchor() {
        let v = parse("first: &a 7\nsecond: *a\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("first")).unwrap().as_int(), Some(7));
        assert_eq!(m.get(&key("second")).unwrap().as_int(), Some(7));
    }

    #[test]
    fn alias_clones_collection_anchor() {
        let v = parse("a: &seq [1, 2]\nb: *seq\n");
        let m = v.as_mapping().unwrap();
        let b = m.get(&key("b")).unwrap().as_sequence().unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[1].as_int(), Some(2));
    }

    #[test]
    fn unknown_alias_is_compose_error() {
        let err = crate::api::parse("*missing\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn anchors_do_not_leak_across_documents() {
        // `*a` in the second document must not see the first document's anchor.
        let err = crate::api::parse_documents("--- &a 1\n--- *a\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }
}
