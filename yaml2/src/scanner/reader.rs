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
        Self {
            input,
            offset: 0,
            line: 1,
            column: 1,
        }
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

    /// The character `n` positions ahead without consuming (0 == next).
    pub(crate) fn peek_nth(&self, n: usize) -> Option<char> {
        self.input[self.offset..].chars().nth(n)
    }

    /// Whether the remaining input begins with `prefix`.
    pub(crate) fn starts_with(&self, prefix: &str) -> bool {
        self.input[self.offset..].starts_with(prefix)
    }

    /// Consume and return the next character, updating offset/line/column.
    ///
    /// Recognizes YAML line breaks `\n`, `\r\n`, and lone `\r` for line
    /// counting; the offset always advances by the char's UTF-8 length.
    pub(crate) fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        match c {
            '\n' => {
                self.line += 1;
                self.column = 1;
            }
            '\r' => {
                // A lone CR is a line break. In a CRLF pair the CR is just the
                // terminator prefix (no column change); the following '\n'
                // performs the actual line break.
                if self.peek() != Some('\n') {
                    self.line += 1;
                    self.column = 1;
                }
            }
            _ => {
                self.column += 1;
            }
        }
        Some(c)
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

    #[test]
    fn advance_returns_and_consumes_chars() {
        let mut r = Reader::new("ab");
        assert_eq!(r.advance(), Some('a'));
        assert_eq!(r.advance(), Some('b'));
        assert_eq!(r.advance(), None);
        assert!(r.is_eof());
    }

    #[test]
    fn advance_tracks_column_and_offset() {
        let mut r = Reader::new("ab");
        r.advance();
        assert_eq!(r.position(), Position::new(1, 1, 2));
    }

    #[test]
    fn newline_advances_line_and_resets_column() {
        let mut r = Reader::new("a\nb");
        r.advance(); // a
        r.advance(); // \n
        assert_eq!(r.position(), Position::new(2, 2, 1));
        assert_eq!(r.peek(), Some('b'));
    }

    #[test]
    fn crlf_counts_as_one_line_break() {
        let mut r = Reader::new("a\r\nb");
        r.advance(); // a
        r.advance(); // \r
        r.advance(); // \n
        assert_eq!(r.position(), Position::new(3, 2, 1));
        assert_eq!(r.peek(), Some('b'));
    }

    #[test]
    fn lone_cr_at_eof_is_a_line_break() {
        let mut r = Reader::new("a\r");
        r.advance(); // a
        r.advance(); // \r at EOF -> lone CR
        assert_eq!(r.position().line, 2);
        assert_eq!(r.position().column, 1);
        assert!(r.is_eof());
    }

    #[test]
    fn lone_cr_is_a_line_break() {
        let mut r = Reader::new("a\rb");
        r.advance(); // a
        r.advance(); // \r
        assert_eq!(r.position().line, 2);
        assert_eq!(r.position().column, 1);
    }

    #[test]
    fn multibyte_char_advances_offset_by_utf8_len() {
        let mut r = Reader::new("é!"); // 'é' is 2 bytes
        r.advance();
        assert_eq!(r.position(), Position::new(2, 1, 2));
        assert_eq!(r.peek(), Some('!'));
    }

    #[test]
    fn peek_nth_and_starts_with() {
        let r = Reader::new("abc");
        assert_eq!(r.peek_nth(0), Some('a'));
        assert_eq!(r.peek_nth(2), Some('c'));
        assert_eq!(r.peek_nth(3), None);
        assert!(r.starts_with("abc"));
        assert!(!r.starts_with("abd"));
    }
}
