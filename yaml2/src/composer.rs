//! The composer: events -> owned `Value` documents.

use std::collections::HashMap;

use crate::error::{Error, ErrorKind, Result, Span};
use crate::event::{Event, EventKind};
use crate::meta::ScalarStyle;
use crate::options::{ParseOptions, Schema};
use crate::value::{Mapping, Value, ValueData};

/// Composes a validated event stream into one `Value` per document.
pub(crate) fn compose(events: &[Event], options: &ParseOptions) -> Result<Vec<Value>> {
    let mut composer = Composer {
        events,
        pos: 0,
        options,
        anchors: HashMap::new(),
        alias_nodes: 0,
        merge_enabled: options.merge_keys.enabled_for(options.schema),
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
    merge_enabled: bool,
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
        // Merge sources, in source order; applied after all explicit keys.
        let mut merges: Vec<Mapping> = Vec::new();
        loop {
            match self.peek() {
                Some(EventKind::MappingEnd) => {
                    self.bump()?;
                    break;
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "unterminated mapping in event stream",
                    ))
                }
                Some(_) => {
                    if self.merge_enabled && self.peek_is_merge_key() {
                        let span = self.events[self.pos].span;
                        self.bump()?; // consume the `<<` key event
                        let value = self.compose_node()?;
                        self.collect_merge_sources(value, &mut merges, span)?;
                    } else {
                        let k = self.compose_node()?;
                        let val = self.compose_node()?;
                        map.insert(k, val);
                    }
                }
            }
        }
        // Apply merges: explicit keys (and earlier sources) win.
        for source in merges {
            for (k, v) in source.iter() {
                if !map.contains_key(k) {
                    map.insert(k.clone(), v.clone());
                }
            }
        }
        let value = Value::mapping(map);
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        Ok(value)
    }

    /// True if the next event is a plain, untagged `<<` scalar key.
    fn peek_is_merge_key(&self) -> bool {
        match self.events.get(self.pos).map(|e| &e.kind) {
            Some(EventKind::Scalar {
                value,
                style: ScalarStyle::Plain,
                tag: None,
                anchor: None,
            }) => value == "<<",
            _ => false,
        }
    }

    /// Flattens a merge value into ordered mapping sources. The value is a
    /// mapping, or a sequence of mappings.
    fn collect_merge_sources(
        &self,
        value: Value,
        merges: &mut Vec<Mapping>,
        span: Span,
    ) -> Result<()> {
        match value.into_data() {
            ValueData::Mapping(m) => merges.push(m),
            ValueData::Sequence(items) => {
                for item in items {
                    match item.into_data() {
                        ValueData::Mapping(m) => merges.push(m),
                        _ => return Err(self.error(span, "merge sequence entry must be a mapping")),
                    }
                }
            }
            _ => {
                return Err(self.error(
                    span,
                    "merge value must be a mapping or a sequence of mappings",
                ))
            }
        }
        Ok(())
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

    /// Resolves a scalar event into a typed `Value`, honoring an explicit core
    /// tag if present. Untagged scalars use the configured schema. The
    /// non-specific `!` and unknown/custom tags resolve to strings.
    fn resolve_scalar(
        &self,
        raw: &str,
        style: ScalarStyle,
        tag: Option<&str>,
        span: Span,
    ) -> Result<Value> {
        let Some(tag) = tag else {
            return Ok(Value::from_scalar(raw, style, self.options.schema));
        };
        match classify_tag(tag) {
            Some(CoreTag::Str) => Ok(Value::string(raw)),
            Some(CoreTag::Null) => match typed(raw) {
                ValueData::Null => Ok(Value::null()),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!null"))),
            },
            Some(CoreTag::Bool) => match typed(raw) {
                ValueData::Bool(b) => Ok(Value::bool(b)),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!bool"))),
            },
            Some(CoreTag::Int) => match typed(raw) {
                ValueData::Int(i) => Ok(Value::int(i)),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!int"))),
            },
            Some(CoreTag::Float) => match typed(raw) {
                ValueData::Float(f) => Ok(Value::float(f)),
                ValueData::Int(i) => Ok(Value::float(i as f64)),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!float"))),
            },
            Some(CoreTag::Seq) | Some(CoreTag::Map) => Err(self.error(
                span,
                format!("collection tag '{tag}' applied to a scalar node"),
            )),
            // Non-specific `!` or any unknown/custom tag: treat as a string.
            None => Ok(Value::string(raw)),
        }
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

/// The seven core schema tags this layer understands on scalar nodes.
enum CoreTag {
    Str,
    Null,
    Bool,
    Int,
    Float,
    Seq,
    Map,
}

/// Maps a raw tag string (shorthand `!!x` or verbatim `tag:yaml.org,2002:x`) to
/// a core tag, or `None` for the non-specific `!` and unknown/custom tags.
fn classify_tag(tag: &str) -> Option<CoreTag> {
    match tag {
        "!!str" | "tag:yaml.org,2002:str" => Some(CoreTag::Str),
        "!!null" | "tag:yaml.org,2002:null" => Some(CoreTag::Null),
        "!!bool" | "tag:yaml.org,2002:bool" => Some(CoreTag::Bool),
        "!!int" | "tag:yaml.org,2002:int" => Some(CoreTag::Int),
        "!!float" | "tag:yaml.org,2002:float" => Some(CoreTag::Float),
        "!!seq" | "tag:yaml.org,2002:seq" => Some(CoreTag::Seq),
        "!!map" | "tag:yaml.org,2002:map" => Some(CoreTag::Map),
        _ => None,
    }
}

/// Resolves raw text under the Core 1.2 schema for tag-driven typing. A core
/// tag explicitly requests its type, so we type the text with core rules
/// regardless of the document's configured schema.
fn typed(raw: &str) -> ValueData {
    crate::scalar::resolve(raw, ScalarStyle::Plain, Schema::Core1_2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Limits, MergeKeys};

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

    fn parse_merge(input: &str) -> Value {
        // Merge keys on, Core scalar resolution (numbers resolve plainly).
        let opts = ParseOptions {
            merge_keys: MergeKeys::On,
            ..Default::default()
        };
        crate::api::parse_with(input, &opts).unwrap()
    }

    #[test]
    fn merge_pulls_in_anchor_entries() {
        let input = "\
base: &b
  a: 1
  b: 2
derived:
  <<: *b
  c: 3
";
        let m = parse_merge(input);
        let d = m.as_mapping().unwrap().get(&key("derived")).unwrap();
        let d = d.as_mapping().unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(d.get(&key("b")).unwrap().as_int(), Some(2));
        assert_eq!(d.get(&key("c")).unwrap().as_int(), Some(3));
        assert!(!d.contains_key(&key("<<")));
    }

    #[test]
    fn explicit_key_overrides_merged() {
        let input = "\
base: &b
  a: 1
  b: 2
derived:
  <<: *b
  b: 99
";
        let m = parse_merge(input);
        let d = m.as_mapping().unwrap().get(&key("derived")).unwrap();
        let d = d.as_mapping().unwrap();
        assert_eq!(d.get(&key("b")).unwrap().as_int(), Some(99));
        assert_eq!(d.get(&key("a")).unwrap().as_int(), Some(1));
    }

    #[test]
    fn merge_sequence_earlier_source_wins() {
        let input = "\
one: &one {a: 1, x: 10}
two: &two {a: 2, y: 20}
merged:
  <<: [*one, *two]
";
        let m = parse_merge(input);
        let d = m.as_mapping().unwrap().get(&key("merged")).unwrap();
        let d = d.as_mapping().unwrap();
        // `a` present in both; earlier source (*one) wins.
        assert_eq!(d.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(d.get(&key("x")).unwrap().as_int(), Some(10));
        assert_eq!(d.get(&key("y")).unwrap().as_int(), Some(20));
    }

    #[test]
    fn merge_disabled_keeps_literal_key() {
        // Default options: Core schema, merge keys Auto -> off.
        let v = parse("<<: {a: 1}\nb: 2\n");
        let m = v.as_mapping().unwrap();
        assert!(m.contains_key(&key("<<")));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn quoted_merge_key_is_literal_even_when_enabled() {
        let v = parse_merge("\"<<\": 5\nb: 2\n");
        let m = v.as_mapping().unwrap();
        assert!(m.contains_key(&key("<<")));
        assert_eq!(m.get(&key("<<")).unwrap().as_int(), Some(5));
    }

    #[test]
    fn merge_non_mapping_value_is_error() {
        let err = {
            let opts = ParseOptions {
                merge_keys: MergeKeys::On,
                ..Default::default()
            };
            crate::api::parse_with("<<: 5\n", &opts).unwrap_err()
        };
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
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

    #[test]
    fn aliases_within_budget_succeed() {
        // Two aliases of a 2-element sequence: well under the default budget.
        let v = parse("base: &b [1, 2]\nx: *b\ny: *b\n");
        assert!(v.as_mapping().unwrap().get(&key("y")).is_some());
    }

    #[test]
    fn nested_alias_expansion_exceeds_small_budget() {
        // Classic billion-laughs shape: each level references the previous one
        // several times, so materialized node count explodes.
        let input = "\
a: &a [x, x, x, x, x]
b: &b [*a, *a, *a, *a, *a]
c: &c [*b, *b, *b, *b, *b]
d: &d [*c, *c, *c, *c, *c]
e: [*d, *d, *d, *d, *d]
";
        let opts = ParseOptions {
            limits: Limits {
                max_aliases: 1_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let err = crate::api::parse_with(input, &opts).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::LimitExceeded);
    }

    #[test]
    fn generous_budget_allows_the_same_input() {
        let input = "\
a: &a [x, x]
b: [*a, *a]
";
        let opts = ParseOptions {
            limits: Limits {
                max_aliases: 1_000_000,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(crate::api::parse_with(input, &opts).is_ok());
    }

    #[test]
    fn str_tag_forces_string() {
        assert_eq!(parse("!!str 123\n").as_str(), Some("123"));
    }

    #[test]
    fn int_tag_on_quoted_value() {
        assert_eq!(parse("!!int \"5\"\n").as_int(), Some(5));
    }

    #[test]
    fn float_tag_widens_integer_text() {
        assert_eq!(parse("!!float 5\n").as_float(), Some(5.0));
    }

    #[test]
    fn bool_tag_parses_keyword() {
        assert_eq!(parse("!!bool true\n").as_bool(), Some(true));
    }

    #[test]
    fn null_tag_parses_tilde() {
        assert!(parse("!!null ~\n").is_null());
    }

    #[test]
    fn mismatched_int_tag_is_error() {
        let err = crate::api::parse("!!int notanumber\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn collection_tag_on_scalar_is_error() {
        let err = crate::api::parse("!!map scalar\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn unknown_tag_on_scalar_resolves_to_string() {
        assert_eq!(parse("!custom 42\n").as_str(), Some("42"));
    }

    #[test]
    fn non_specific_tag_forces_string() {
        assert_eq!(parse("! 42\n").as_str(), Some("42"));
    }

    #[test]
    fn core_shorthand_tag_still_types() {
        // Regression: the resolved `tag:yaml.org,2002:int` types as an int.
        assert_eq!(parse("!!int 5\n").as_int(), Some(5));
    }

    #[test]
    fn verbatim_core_tag_types() {
        assert_eq!(parse("!<tag:yaml.org,2002:int> 5\n").as_int(), Some(5));
        assert_eq!(parse("!<tag:yaml.org,2002:str> 5\n").as_str(), Some("5"));
    }

    #[test]
    fn local_tag_resolves_to_string() {
        // A local `!lang` tag leaves the scalar a string.
        assert_eq!(parse("!lang hello\n").as_str(), Some("hello"));
    }

    #[test]
    fn custom_directive_tag_resolves_to_string() {
        let input = "%TAG !e! tag:example.com,2000:\n--- !e!color red\n";
        assert_eq!(parse(input).as_str(), Some("red"));
    }

    #[test]
    fn directive_scoped_typing_in_mapping() {
        // A core tag via the default secondary handle still types inside a map.
        let v = parse("count: !!int 3\nname: !!str 7\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("count")).unwrap().as_int(), Some(3));
        assert_eq!(m.get(&key("name")).unwrap().as_str(), Some("7"));
    }

    #[test]
    fn realistic_document_tree() {
        let input = "\
name: Ada
active: true
scores: [10, 20, 30]
address:
  city: Portland
  zip: \"97201\"
";
        let v = parse(input);
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("name")).unwrap().as_str(), Some("Ada"));
        assert_eq!(m.get(&key("active")).unwrap().as_bool(), Some(true));
        let scores = m.get(&key("scores")).unwrap().as_sequence().unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[1].as_int(), Some(20));
        let addr = m.get(&key("address")).unwrap().as_mapping().unwrap();
        assert_eq!(addr.get(&key("city")).unwrap().as_str(), Some("Portland"));
        // Quoted zip stays a string, not an int.
        assert_eq!(addr.get(&key("zip")).unwrap().as_str(), Some("97201"));
    }

    #[test]
    fn anchors_aliases_and_merge_together() {
        let input = "\
defaults: &d
  retries: 3
  timeout: 30
job:
  <<: *d
  timeout: 60
";
        let opts = ParseOptions {
            merge_keys: MergeKeys::On,
            ..Default::default()
        };
        let v = crate::api::parse_with(input, &opts).unwrap();
        let job = v.as_mapping().unwrap().get(&key("job")).unwrap();
        let job = job.as_mapping().unwrap();
        assert_eq!(job.get(&key("retries")).unwrap().as_int(), Some(3));
        assert_eq!(job.get(&key("timeout")).unwrap().as_int(), Some(60));
    }

    #[test]
    fn multiline_plain_value_composes_folded() {
        let v = parse("summary: line one\n  line two\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(
            m.get(&key("summary")).unwrap().as_str(),
            Some("line one line two")
        );
    }

    #[test]
    fn multiline_plain_blank_line_becomes_newline() {
        let v = parse("text: para one\n\n  para two\n");
        assert_eq!(
            v.as_mapping().unwrap().get(&key("text")).unwrap().as_str(),
            Some("para one\npara two")
        );
    }

    #[test]
    fn multiline_plain_root_document() {
        assert_eq!(parse("one\ntwo\nthree\n").as_str(), Some("one two three"));
    }

    #[test]
    fn multiline_plain_in_flow_sequence_composes() {
        let v = parse("[one\n two, three]\n");
        let items = v.as_sequence().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("one two"));
        assert_eq!(items[1].as_str(), Some("three"));
    }

    #[test]
    fn schema_changes_scalar_typing() {
        // Under Yaml1_1, "yes" is a bool; under Core it is a string.
        let yes_core = parse("yes\n");
        assert_eq!(yes_core.as_str(), Some("yes"));

        let opts = ParseOptions {
            schema: crate::options::Schema::Yaml1_1,
            ..Default::default()
        };
        let yes_11 = crate::api::parse_with("yes\n", &opts).unwrap();
        assert_eq!(yes_11.as_bool(), Some(true));
    }

    #[test]
    fn tab_indented_document_is_an_error() {
        let err = crate::api::parse("root:\n\tchild: v\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn space_indented_document_still_parses() {
        // Regression: ordinary space indentation is unaffected.
        let v = crate::api::parse("root:\n  child: v\n").unwrap();
        let m = v.as_mapping().unwrap();
        let child = m.get(&key("root")).unwrap().as_mapping().unwrap();
        assert_eq!(child.get(&key("child")).unwrap().as_str(), Some("v"));
    }
}
