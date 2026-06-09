//! Error, source-position, and span types.

/// A location in the source: byte offset (0-based), line and column (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    fn span_exposes_start_and_end() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(5, 1, 6));
        assert_eq!(span.start.column, 1);
        assert_eq!(span.end.offset, 5);
    }
}
