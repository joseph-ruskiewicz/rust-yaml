//! Error, source-position, and span types.

use core::fmt;

/// The category of a parse failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Scan,
    Parse,
    Compose,
    LimitExceeded,
}

/// A YAML processing error, optionally carrying a source span.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    span: Option<Span>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), span: None }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn span(&self) -> Option<Span> {
        self.span
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(s) => write!(f, "{} at line {} column {}", self.message, s.start.line, s.start.column),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A location in the source: byte offset (0-based), line and column (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl Position {
    pub fn new(offset: usize, line: usize, column: usize) -> Self {
        Self { offset, line, column }
    }

    /// The position at the very start of any input.
    pub fn start() -> Self {
        Self { offset: 0, line: 1, column: 1 }
    }
}

impl Default for Position {
    fn default() -> Self {
        Self::start()
    }
}

/// A half-open range in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_start_is_line_one_column_one() {
        let p = Position::start();
        assert_eq!(p.offset, 0);
        assert_eq!(p.line, 1);
        assert_eq!(p.column, 1);
    }

    #[test]
    fn position_default_equals_start() {
        assert_eq!(Position::default(), Position::start());
        assert_eq!(Span::default().start, Position::start());
    }

    #[test]
    fn span_exposes_start_and_end() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(5, 1, 6));
        assert_eq!(span.start.column, 1);
        assert_eq!(span.end.offset, 5);
    }

    #[test]
    fn error_without_span_displays_message_only() {
        let e = Error::new(ErrorKind::Parse, "unexpected token");
        assert_eq!(e.to_string(), "unexpected token");
        assert_eq!(e.kind(), ErrorKind::Parse);
        assert!(e.span().is_none());
    }

    #[test]
    fn error_with_span_displays_location() {
        let span = Span::new(Position::new(3, 2, 1), Position::new(4, 2, 2));
        let e = Error::new(ErrorKind::Scan, "bad indent").with_span(span);
        assert_eq!(e.to_string(), "bad indent at line 2 column 1");
        assert_eq!(e.span(), Some(span));
    }
}
