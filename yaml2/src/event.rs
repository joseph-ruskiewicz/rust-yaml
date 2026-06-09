//! The public event model produced by the parser.

use crate::error::Span;
use crate::meta::ScalarStyle;

/// A parser event with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub span: Span,
}

impl Event {
    pub fn new(kind: EventKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The kinds of events the parser emits (the YAML event model).
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    /// A scalar node. `anchor`/`tag` are the node's properties if present.
    Scalar {
        value: String,
        style: ScalarStyle,
        anchor: Option<String>,
        tag: Option<String>,
    },
    /// An alias reference (`*name`).
    Alias(String),
    SequenceStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    SequenceEnd,
    MappingStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    MappingEnd,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Position;

    #[test]
    fn event_holds_kind_and_span() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(0, 1, 1));
        let e = Event::new(EventKind::StreamStart, span);
        assert_eq!(e.kind, EventKind::StreamStart);
        assert_eq!(e.span, span);
    }

    #[test]
    fn scalar_event_carries_value_style_anchor_tag() {
        let kind = EventKind::Scalar {
            value: "x".to_string(),
            style: ScalarStyle::Plain,
            anchor: Some("a".to_string()),
            tag: Some("!t".to_string()),
        };
        assert_eq!(
            kind,
            EventKind::Scalar {
                value: "x".to_string(),
                style: ScalarStyle::Plain,
                anchor: Some("a".to_string()),
                tag: Some("!t".to_string()),
            }
        );
    }
}
