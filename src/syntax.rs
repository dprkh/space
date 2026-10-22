use std::sync::LazyLock;

use gpui::{FontStyle, FontWeight, HighlightStyle, UnderlineStyle, px, rgba};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle as SyntaxFontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

pub fn python(source: &str) -> Option<Vec<(usize, HighlightStyle)>> {
    static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    static THEME: LazyLock<Theme> = LazyLock::new(|| {
        ThemeSet::load_defaults()
            .themes
            .remove("base16-ocean.dark")
            .unwrap_or_default()
    });

    let syntax = SYNTAXES.find_syntax_by_extension("py")?;
    let mut highlighter = HighlightLines::new(syntax, &THEME);
    let mut runs: Vec<(usize, HighlightStyle)> = Vec::new();
    for line in LinesWithEndings::from(source) {
        for (style, text) in highlighter.highlight_line(line, &SYNTAXES).ok()? {
            if text.is_empty() {
                continue;
            }
            let color = style.foreground;
            let highlight = HighlightStyle {
                color: Some(rgba(u32::from_be_bytes([color.r, color.g, color.b, color.a])).into()),
                font_weight: style
                    .font_style
                    .contains(SyntaxFontStyle::BOLD)
                    .then_some(FontWeight::BOLD),
                font_style: style
                    .font_style
                    .contains(SyntaxFontStyle::ITALIC)
                    .then_some(FontStyle::Italic),
                underline: style
                    .font_style
                    .contains(SyntaxFontStyle::UNDERLINE)
                    .then_some(UnderlineStyle {
                        thickness: px(1.),
                        ..Default::default()
                    }),
                ..Default::default()
            };
            if let Some((len, previous)) = runs.last_mut()
                && *previous == highlight
            {
                *len += text.len();
            } else {
                runs.push((text.len(), highlight));
            }
        }
    }
    Some(runs)
}
