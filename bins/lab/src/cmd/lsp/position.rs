//! Byte ⇄ LSP position mapping for one document revision.
//!
//! Spans produced by the NLL lexer and parser are byte ranges into the
//! source. LSP positions are (line, character) pairs where `character`
//! counts **UTF-16 code units**, which is the protocol default and what
//! this server advertises (it leaves `positionEncoding` unset). The two
//! only agree on ASCII, so every span crossing the wire goes through
//! [`LineIndex`].

use tower_lsp_server::ls_types::{Position, Range};

/// Line table for one revision of a document.
pub struct LineIndex {
    text: String,
    /// Byte offset of the first byte of each line. Always starts with 0.
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes().enumerate().filter_map(
                |(i, b)| {
                    if b == b'\n' { Some(i + 1) } else { None }
                },
            ),
        );
        Self {
            text: text.to_string(),
            line_starts,
        }
    }

    /// UTF-16 position of a byte offset.
    ///
    /// An offset past the end of the document clamps to its end; an
    /// offset inside a multi-byte character snaps back to that
    /// character's first byte (spans from the lexer are always
    /// boundaries, so this only matters for clamped inputs).
    pub fn position(&self, byte: usize) -> Position {
        let byte = byte.min(self.text.len());
        let line = self.line_starts.partition_point(|&start| start <= byte) - 1;
        let line_start = self.line_starts[line];
        // Walk back to a char boundary rather than panicking on a slice.
        let mut end = byte;
        while end > line_start && !self.text.is_char_boundary(end) {
            end -= 1;
        }
        let character = self.text[line_start..end].encode_utf16().count();
        Position {
            line: line as u32,
            character: character as u32,
        }
    }

    /// Byte offset of a UTF-16 position.
    ///
    /// A `character` past the end of its line clamps to the line's end
    /// (excluding the newline); a `line` past the end of the document
    /// clamps to the end of the document. A character landing inside a
    /// surrogate pair snaps to the start of that pair.
    pub fn offset(&self, pos: Position) -> usize {
        let Some(&line_start) = self.line_starts.get(pos.line as usize) else {
            return self.text.len();
        };
        let line_end = self
            .line_starts
            .get(pos.line as usize + 1)
            .map(|&next| next - 1) // drop the '\n' that ends the line
            .unwrap_or(self.text.len());
        let line = &self.text[line_start..line_end];
        let mut units = 0usize;
        for (offset, ch) in line.char_indices() {
            // `>` and not `>=`: a character landing inside a surrogate
            // pair snaps to the start of that pair.
            if units + ch.len_utf16() > pos.character as usize {
                return line_start + offset;
            }
            units += ch.len_utf16();
        }
        line_end
    }

    /// Range covering a byte span.
    ///
    /// A zero-width span in the middle of the document is widened to one
    /// character so the editor draws something; a zero-width span at the
    /// end of input (what an unexpected-EOF parse error carries) stays
    /// empty. This mirrors what `attach_source` does for miette.
    pub fn range(&self, span: std::ops::Range<usize>) -> Range {
        let start = span.start.min(self.text.len());
        let mut end = span.end.max(start).min(self.text.len());
        if end == start && start < self.text.len() {
            end = start + self.text[start..].chars().next().map_or(0, char::len_utf8);
        }
        Range {
            start: self.position(start),
            end: self.position(end),
        }
    }

    /// Range covering the whole document, for a full-document edit.
    pub fn full_range(&self) -> Range {
        Range {
            start: Position::new(0, 0),
            end: self.position(self.text.len()),
        }
    }

    /// Range of the first line, used when a diagnostic has no span at all.
    pub fn first_line(&self) -> Range {
        let end = self.line_starts.get(1).map_or(self.text.len(), |&n| n - 1);
        self.range(0..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `é` is 2 bytes / 1 UTF-16 unit, `🚀` is 4 bytes / 2 units.
    const SRC: &str = "lab \"é\"\nnode 🚀 { }\nx";

    #[test]
    fn position_of_ascii_offsets() {
        let ix = LineIndex::new(SRC);
        assert_eq!(ix.position(0), Position::new(0, 0));
        assert_eq!(ix.position(4), Position::new(0, 4));
    }

    #[test]
    fn position_counts_utf16_units_for_multibyte() {
        let ix = LineIndex::new(SRC);
        // byte 5 is the start of `é`; byte 7 is the closing quote.
        assert_eq!(ix.position(5), Position::new(0, 5));
        assert_eq!(ix.position(7), Position::new(0, 6));
    }

    #[test]
    fn position_counts_surrogate_pairs_as_two_units() {
        let ix = LineIndex::new(SRC);
        let rocket = SRC.find('🚀').unwrap();
        assert_eq!(ix.position(rocket), Position::new(1, 5));
        // Just after the rocket: 5 + 2 UTF-16 units.
        assert_eq!(ix.position(rocket + 4), Position::new(1, 7));
    }

    #[test]
    fn position_clamps_offset_past_end_of_file() {
        let ix = LineIndex::new(SRC);
        assert_eq!(ix.position(9999), ix.position(SRC.len()));
    }

    #[test]
    fn position_snaps_to_char_boundary() {
        let ix = LineIndex::new(SRC);
        let rocket = SRC.find('🚀').unwrap();
        // Mid-character offsets never panic and never move past the char.
        for off in 1..4 {
            assert_eq!(ix.position(rocket + off), ix.position(rocket));
        }
    }

    #[test]
    fn offset_round_trips_through_position() {
        let ix = LineIndex::new(SRC);
        for (byte, _) in SRC.char_indices() {
            assert_eq!(ix.offset(ix.position(byte)), byte, "byte {byte}");
        }
    }

    #[test]
    fn offset_clamps_character_past_end_of_line() {
        let ix = LineIndex::new(SRC);
        // Line 0 is `lab "é"` = 8 bytes, then the newline.
        assert_eq!(ix.offset(Position::new(0, 999)), 8);
    }

    #[test]
    fn offset_of_line_past_end_is_document_end() {
        let ix = LineIndex::new(SRC);
        assert_eq!(ix.offset(Position::new(99, 0)), SRC.len());
    }

    #[test]
    fn offset_snaps_into_a_surrogate_pair() {
        let ix = LineIndex::new(SRC);
        let rocket = SRC.find('🚀').unwrap();
        // Character 6 is the low half of the pair: snap to its start.
        assert_eq!(ix.offset(Position::new(1, 6)), rocket);
    }

    #[test]
    fn crlf_does_not_shift_line_numbers() {
        let ix = LineIndex::new("a\r\nb");
        assert_eq!(ix.position(3), Position::new(1, 0));
    }

    #[test]
    fn range_widens_a_zero_width_span_but_not_at_eof() {
        let ix = LineIndex::new(SRC);
        let mid = ix.range(0..0);
        assert_eq!(mid.start, Position::new(0, 0));
        assert_eq!(mid.end, Position::new(0, 1));
        let eof = ix.range(SRC.len()..SRC.len());
        assert_eq!(eof.start, eof.end);
    }

    #[test]
    fn full_range_and_first_line_cover_what_they_say() {
        let ix = LineIndex::new(SRC);
        assert_eq!(ix.full_range().start, Position::new(0, 0));
        assert_eq!(ix.full_range().end, ix.position(SRC.len()));
        assert_eq!(ix.first_line().end, Position::new(0, 7));
    }

    #[test]
    fn empty_document_is_a_single_empty_line() {
        let ix = LineIndex::new("");
        assert_eq!(ix.position(0), Position::new(0, 0));
        assert_eq!(ix.range(0..0).start, ix.range(0..0).end);
        assert_eq!(ix.first_line().end, Position::new(0, 0));
    }
}
