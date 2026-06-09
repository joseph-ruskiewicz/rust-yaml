//! A character cursor over the input with source-position tracking.

use crate::error::Position;

pub(crate) struct Reader<'a> {
    input: &'a str,
    offset: usize,
    line: usize,
    column: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(input: &'a str) -> Self {
        Self { input, offset: 0, line: 1, column: 1 }
    }

    /// Current source position.
    pub(crate) fn position(&self) -> Position {
        Position::new(self.offset, self.line, self.column)
    }

    /// Total length of the input in bytes.
    pub(crate) fn input_len(&self) -> usize {
        self.input.len()
    }

    pub(crate) fn is_eof(&self) -> bool {
        self.offset >= self.input.len()
    }

    /// The next character without consuming it.
    pub(crate) fn peek(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reader_is_at_start() {
        let r = Reader::new("abc");
        assert_eq!(r.position(), Position::new(0, 1, 1));
        assert!(!r.is_eof());
    }

    #[test]
    fn peek_does_not_consume() {
        let r = Reader::new("abc");
        assert_eq!(r.peek(), Some('a'));
        assert_eq!(r.peek(), Some('a'));
    }

    #[test]
    fn empty_input_is_eof() {
        let r = Reader::new("");
        assert!(r.is_eof());
        assert_eq!(r.peek(), None);
    }
}
