//! A borderless table: left-aligned columns two spaces apart, widths
//! measured in terminal cells (styles and wide characters accounted for),
//! one column shrinking to fit the terminal.

use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    right: Vec<bool>,
}

/// The width a cell takes on screen: escapes count for nothing.
fn cells(s: &str) -> usize {
    anstream::adapter::strip_str(s).to_string().width()
}

/// `s` cut to `max` cells, ending in `…` when it was cut. A styled cell
/// that is cut loses its style.
fn cut(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_string();
    }
    let plain = anstream::adapter::strip_str(s).to_string();
    let mut out = String::new();
    let mut used = 0;
    for c in plain.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

impl Table {
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|h| h.to_string()).collect(),
            right: vec![false; headers.len()],
            ..Default::default()
        }
    }

    /// Right-align column `col` (numbers).
    pub fn right(mut self, col: usize) -> Self {
        if let Some(r) = self.right.get_mut(col) {
            *r = true;
        }
        self
    }

    /// A row; missing cells are empty, extra ones dropped.
    pub fn row<I, S>(&mut self, row: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut row: Vec<String> = row.into_iter().map(Into::into).collect();
        row.resize(self.headers.len(), String::new());
        self.rows.push(row);
    }

    /// The table as lines, fitted to `width` cells when it is given.
    pub fn render(&self, width: Option<usize>) -> String {
        let n = self.headers.len();
        let mut widths: Vec<usize> = (0..n)
            .map(|c| {
                std::iter::once(&self.headers[c])
                    .chain(self.rows.iter().map(|r| &r[c]))
                    .map(|s| cells(s))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        // the last column is cut to fit
        let flex = n.saturating_sub(1);
        if let Some(width) = width {
            let total: usize = widths.iter().sum::<usize>() + 2 * n.saturating_sub(1);
            if total > width && n > 0 {
                let others = total - widths[flex];
                // never below the header, or a few cells
                let floor = cells(&self.headers[flex]).max(4);
                widths[flex] = width.saturating_sub(others).max(floor);
            }
        }
        let mut out = String::new();
        let header: Vec<String> = self.headers.clone();
        for row in std::iter::once(&header).chain(self.rows.iter()) {
            let mut line = String::new();
            for (c, cell) in row.iter().enumerate() {
                let cell = if c == flex {
                    cut(cell, widths[c])
                } else {
                    cell.clone()
                };
                let pad = widths[c].saturating_sub(cells(&cell));
                if c > 0 {
                    line.push_str("  ");
                }
                if self.right[c] {
                    line.push_str(&" ".repeat(pad));
                    line.push_str(&cell);
                } else {
                    line.push_str(&cell);
                    if c + 1 < n {
                        line.push_str(&" ".repeat(pad));
                    }
                }
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }
}

/// The width tables fit (`Table::render`): `COLUMNS`, else the
/// terminal's when stdout is a terminal, else none (a pipe gets whole
/// cells).
pub fn terminal_width() -> Option<usize> {
    use std::io::IsTerminal;
    if let Some(cols) = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
    {
        return Some(cols);
    }
    if !std::io::stdout().is_terminal() {
        return None;
    }
    crossterm::terminal::size().ok().map(|(w, _)| w as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Table {
        let mut t = Table::new(&["NODE", "PID", "COMMAND"]).right(1);
        t.row(["router", "4242", "/usr/sbin/bird -f -c /etc/bird.conf"]);
        t.row(["h1", "17", "sleep infinity"]);
        t
    }

    #[test]
    fn columns_align_and_numbers_right() {
        assert_eq!(
            sample().render(None),
            "NODE     PID  COMMAND\n\
             router  4242  /usr/sbin/bird -f -c /etc/bird.conf\n\
             h1        17  sleep infinity\n"
        );
    }

    #[test]
    fn the_flex_column_is_cut_to_the_width() {
        let out = sample().render(Some(30));
        for line in out.lines() {
            assert!(line.width() <= 30, "{line:?}");
        }
        assert!(out.contains("/usr/sbin/bird…"), "{out}");
        assert!(out.contains("sleep infinity"), "{out}");
    }

    #[test]
    fn styles_and_wide_characters_take_their_screen_width() {
        let mut t = Table::new(&["A", "B"]);
        t.row([crate::output::style::green("ok"), "x".into()]);
        t.row(["日本".to_string(), "y".into()]);
        let out = anstream::adapter::strip_str(&t.render(None)).to_string();
        assert_eq!(out, "A     B\nok    x\n日本  y\n");
    }
}
