//! Colours. `--color auto|always|never` sets the choice once; text is
//! styled with ANSI escapes and printed through `anstream`'s `println!` /
//! `eprintln!`, which strip them per stream: a coloured status on stderr
//! no longer decides whether stdout gets escapes. `auto` honours
//! `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` and `TERM=dumb`.

use anstyle::{AnsiColor, Style};

/// When to colour (`--color`).
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorWhen {
    /// When the stream is a terminal.
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    /// The `--color` given in `args`, before clap has parsed them, so the
    /// help and the usage errors clap prints are coloured to match.
    pub fn from_args(args: &[std::ffi::OsString]) -> Self {
        let mut it = args.iter().filter_map(|a| a.to_str());
        while let Some(a) = it.next() {
            let value = match a.strip_prefix("--color") {
                Some("") => it.next(),
                Some(v) => v.strip_prefix('='),
                None => continue,
            };
            return match value {
                Some("always") => ColorWhen::Always,
                Some("never") => ColorWhen::Never,
                _ => ColorWhen::Auto,
            };
        }
        ColorWhen::Auto
    }

    /// Make it the choice of every stream `anstream` writes.
    pub fn apply(self) {
        match self {
            ColorWhen::Auto => anstream::ColorChoice::Auto,
            ColorWhen::Always => anstream::ColorChoice::Always,
            ColorWhen::Never => anstream::ColorChoice::Never,
        }
        .write_global();
    }

    pub fn clap(self) -> clap::ColorChoice {
        match self {
            ColorWhen::Auto => clap::ColorChoice::Auto,
            ColorWhen::Always => clap::ColorChoice::Always,
            ColorWhen::Never => clap::ColorChoice::Never,
        }
    }
}

fn paint(style: Style, s: &str) -> String {
    format!("{style}{s}{style:#}")
}

pub fn green(s: &str) -> String {
    paint(AnsiColor::Green.on_default(), s)
}

pub fn red(s: &str) -> String {
    paint(AnsiColor::Red.on_default(), s)
}

pub fn yellow(s: &str) -> String {
    paint(AnsiColor::Yellow.on_default(), s)
}

pub fn bold(s: &str) -> String {
    paint(Style::new().bold(), s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_is_read_before_parsing() {
        let args = |v: &[&str]| {
            v.iter()
                .map(Into::into)
                .collect::<Vec<std::ffi::OsString>>()
        };
        assert_eq!(
            ColorWhen::from_args(&args(&["nlink-lab", "--color", "never", "ps"])),
            ColorWhen::Never
        );
        assert_eq!(
            ColorWhen::from_args(&args(&["nlink-lab", "ps", "--color=always"])),
            ColorWhen::Always
        );
        assert_eq!(
            ColorWhen::from_args(&args(&["nlink-lab", "ps"])),
            ColorWhen::Auto
        );
    }

    #[test]
    fn styles_wrap_the_text_and_strip_cleanly() {
        let s = red("ERROR");
        assert_ne!(s, "ERROR");
        assert_eq!(anstream::adapter::strip_str(&s).to_string(), "ERROR");
    }
}
