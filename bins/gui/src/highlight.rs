//! NLL coloured in the editor, a line at a time (iced's `Highlighter`):
//! the lexer's tokens, comments and strings found first, a block comment
//! carried from one line into the next, and the problems' places drawn
//! over it all.

use std::ops::Range;
use std::sync::Arc;

use iced::advanced::text::highlighter::{self, Format};
use iced::{Color, Font, Theme};
use nlink_lab::internal::analysis::symbols;
use nlink_lab::parser::nll::lexer::{Token, lex_lenient};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A statement: `lab`, `node`, `link`, `site`, `for`…
    Keyword,
    /// A block's field: `route`, `delay`, `image`…
    Field,
    /// A name.
    Word,
    /// An address, a CIDR, a duration, a rate, a number.
    Literal,
    Text,
    Comment,
    /// `${…}`.
    Interp,
    Punct,
    /// Where a problem is.
    Error,
    Warning,
}

/// Where the problems are: line, the bytes of it, an error or not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub marks: Arc<Vec<(usize, Range<usize>, bool)>>,
}

/// The line-at-a-time highlighter.
pub struct Nll {
    settings: Settings,
    current: usize,
    /// How deep in block comments each line starts (they nest).
    depth: Vec<usize>,
}

impl highlighter::Highlighter for Nll {
    type Settings = Settings;
    type Highlight = Class;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Class)>;

    fn new(settings: &Settings) -> Self {
        Self {
            settings: settings.clone(),
            current: 0,
            depth: vec![0],
        }
    }

    fn update(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        // the marks moved: everything again
        self.current = 0;
        self.depth.truncate(1);
    }

    fn change_line(&mut self, line: usize) {
        self.current = self.current.min(line);
        self.depth.truncate(self.current + 1);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let depth = self.depth.get(self.current).copied().unwrap_or(0);
        let (mut spans, end) = classes(line, depth);
        for (_, cols, error) in self.settings.marks.iter().filter(|m| m.0 == self.current) {
            let cols = cols.start.min(line.len())..cols.end.min(line.len());
            if !cols.is_empty() {
                spans.push((cols, if *error { Class::Error } else { Class::Warning }));
            }
        }
        self.depth.resize(self.current + 1, 0);
        self.depth.push(end);
        self.current += 1;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

/// A class's look in `theme`.
pub fn format(class: &Class, theme: &Theme) -> Format<Font> {
    let p = theme.extended_palette();
    let dark = p.is_dark;
    let rgb = |l: (u8, u8, u8), d: (u8, u8, u8)| {
        let (r, g, b) = if dark { d } else { l };
        Some(Color::from_rgb8(r, g, b))
    };
    let color = match class {
        Class::Keyword => rgb((0x6f, 0x3d, 0xc8), (0xc5, 0x9b, 0xff)),
        Class::Field => rgb((0x1f, 0x5f, 0xa8), (0x7a, 0xb8, 0xf5)),
        Class::Literal => rgb((0x0b, 0x7a, 0x5a), (0x6e, 0xd3, 0xa8)),
        Class::Text => rgb((0x9a, 0x5b, 0x00), (0xe8, 0xb3, 0x6a)),
        Class::Interp => rgb((0xa8, 0x2e, 0x7a), (0xf0, 0x8c, 0xc8)),
        Class::Comment => Some(Color {
            a: 0.55,
            ..p.background.base.text
        }),
        Class::Error => Some(p.danger.base.color),
        Class::Warning => rgb((0xb3, 0x6b, 0x00), (0xf0, 0xb0, 0x40)),
        Class::Word | Class::Punct => None,
    };
    Format { color, font: None }
}

/// The classes of one line that starts `depth` block comments deep, and
/// how deep it ends.
pub fn classes(line: &str, mut depth: usize) -> (Vec<(Range<usize>, Class)>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    let mut code = 0;
    let at = |i: usize, s: &str| line[i..].starts_with(s);
    // the rest of a block comment, from `start`
    let comment = |start: usize, i: &mut usize, depth: &mut usize, out: &mut Vec<_>| {
        while *i < line.len() && *depth > 0 {
            if at(*i, "/*") {
                *depth += 1;
                *i += 2;
            } else if at(*i, "*/") {
                *depth -= 1;
                *i += 2;
            } else {
                *i += line[*i..].chars().next().map_or(1, char::len_utf8);
            }
        }
        out.push((start..*i, Class::Comment));
    };
    if depth > 0 {
        comment(0, &mut i, &mut depth, &mut out);
        code = i;
    }
    while i < line.len() {
        if at(i, "#") || at(i, "//") {
            tokens(line, code..i, &mut out);
            out.push((i..line.len(), Class::Comment));
            return (out, depth);
        }
        if at(i, "/*") {
            tokens(line, code..i, &mut out);
            let start = i;
            depth = 1;
            i += 2;
            comment(start, &mut i, &mut depth, &mut out);
            code = i;
            continue;
        }
        if at(i, "\"") {
            tokens(line, code..i, &mut out);
            let end = line[i + 1..]
                .find('"')
                .map_or(line.len(), |e| i + 1 + e + 1);
            out.push((i..end, Class::Text));
            i = end;
            code = i;
            continue;
        }
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    tokens(line, code..line.len(), &mut out);
    (out, depth)
}

/// The tokens of `line[range]`, classed.
fn tokens(line: &str, range: Range<usize>, out: &mut Vec<(Range<usize>, Class)>) {
    if range.is_empty() {
        return;
    }
    for t in lex_lenient(&line[range.clone()]) {
        if t.token == Token::Newline {
            continue;
        }
        out.push((
            range.start + t.span.start..range.start + t.span.end,
            class(&t.token),
        ));
    }
}

fn class(t: &Token) -> Class {
    use Token as T;
    match t {
        T::Import
        | T::As
        | T::Lab
        | T::Node
        | T::Profile
        | T::Link
        | T::Network
        | T::Defaults
        | T::Param
        | T::Pool
        | T::Validate
        | T::Scenario
        | T::Benchmark
        | T::Mesh
        | T::Ring
        | T::Star
        | T::For
        | T::In
        | T::Let
        | T::Impair
        | T::Rate => Class::Keyword,
        T::Ident(w) if symbols::STATEMENT_KEYWORDS.iter().any(|(k, _)| k == w) => Class::Keyword,
        T::Ident(w)
            if symbols::BLOCK_KEYWORDS
                .iter()
                .any(|(_, fields)| fields.contains(&w.as_str())) =>
        {
            Class::Field
        }
        T::Ident(_) => Class::Word,
        T::Cidr(_)
        | T::Ipv4Addr(_)
        | T::Ipv6Addr(_)
        | T::Ipv6Cidr(_)
        | T::Duration(_)
        | T::RateLit(_)
        | T::SizeLit(_)
        | T::Percent(_)
        | T::Hex(_)
        | T::Int(_) => Class::Literal,
        T::String(_) => Class::Text,
        T::Interp(_) => Class::Interp,
        _ => Class::Punct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::text::highlighter::Highlighter as _;

    fn named(line: &str, depth: usize) -> (Vec<(String, Class)>, usize) {
        let (spans, end) = classes(line, depth);
        (
            spans
                .into_iter()
                .map(|(r, c)| (line[r].to_string(), c))
                .collect(),
            end,
        )
    }

    #[test]
    fn a_line_is_classed() {
        let (spans, end) = named(
            r#"link r1:eth0 -- r2:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay ${wan} } # wan"#,
            0,
        );
        assert_eq!(end, 0);
        let find = |s: &str| spans.iter().find(|(t, _)| t == s).map(|x| x.1);
        assert_eq!(find("link"), Some(Class::Keyword));
        assert_eq!(find("r1"), Some(Class::Word));
        assert_eq!(find("10.0.0.1/24"), Some(Class::Literal));
        assert_eq!(find("delay"), Some(Class::Field));
        assert_eq!(find("${wan}"), Some(Class::Interp));
        assert_eq!(find("--"), Some(Class::Punct));
        assert_eq!(find("# wan"), Some(Class::Comment));
        let (spans, _) = named(r#"node db { image "postgres:16" } // the db"#, 0);
        assert!(spans.contains(&("\"postgres:16\"".into(), Class::Text)));
        assert!(spans.contains(&("// the db".into(), Class::Comment)));
        assert!(spans.contains(&("image".into(), Class::Field)));
    }

    #[test]
    fn block_comments_carry_over_and_nest() {
        let (spans, end) = named("node a /* one /* two */", 0);
        assert_eq!(end, 1);
        assert_eq!(
            spans.last().unwrap(),
            &("/* one /* two */".into(), Class::Comment)
        );
        let (spans, end) = named("still */ node b", 1);
        assert_eq!(end, 0);
        assert_eq!(spans[0], ("still */".into(), Class::Comment));
        assert!(spans.contains(&("node".into(), Class::Keyword)));
        let (spans, end) = named("all comment", 2);
        assert_eq!((spans.len(), end), (1, 2));
    }

    #[test]
    fn the_highlighter_follows_lines_and_marks() {
        let marks = Settings {
            marks: Arc::new(vec![(1, 5..6, true)]),
        };
        let mut h = Nll::new(&marks);
        let lines = ["/* a", "node b */ node c", "node d"];
        let all: Vec<Vec<(Range<usize>, Class)>> = lines
            .iter()
            .map(|l| h.highlight_line(l).collect())
            .collect();
        assert_eq!(all[0], vec![(0..4, Class::Comment)]);
        assert_eq!(all[1][0], (0..9, Class::Comment));
        assert_eq!(all[1].last().unwrap(), &(5..6, Class::Error));
        assert_eq!(all[2][0], (0..4, Class::Keyword));
        // the first line edited: the next ones start from it again
        h.change_line(0);
        assert_eq!(h.current_line(), 0);
        let first: Vec<_> = h.highlight_line("node a").collect();
        assert_eq!(first[0], (0..4, Class::Keyword));
        let second: Vec<_> = h.highlight_line("node b */ node c").collect();
        assert_eq!(second[0], (0..4, Class::Keyword), "no comment any more");
    }
}
