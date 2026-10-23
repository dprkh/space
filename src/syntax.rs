use std::sync::LazyLock;

use gpui::{FontStyle, FontWeight, HighlightStyle, UnderlineStyle, px, rgba};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle as SyntaxFontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    ThemeSet::load_defaults()
        .themes
        .remove("base16-ocean.dark")
        .unwrap_or_default()
});

pub fn initialize() {
    LazyLock::force(&SYNTAXES);
    LazyLock::force(&THEME);

    // Loading the syntax set leaves its contexts and regular expressions lazy.
    // Parse the Markdown contexts before the first file can be opened.
    let _ = highlight(
        concat!(
            "# Heading\n\n",
            "Paragraph with *emphasis*, **strong**, `code`, and \\*escape.\n",
            "[link](https://example.com) ![image](image.png) <https://example.com>\n\n",
            "> Quote\n\n- List\n1. Numbered list\n\n",
            "Heading\n=======\n\n---\n\n",
            "    Indented code\n\n",
            "```python\nprint(\"code\")\n```\n\n",
            "~~~\ncode\n~~~\n\n",
            "<!-- comment -->\n<div>HTML</div>\n",
        ),
        "md",
    );
}

pub struct Highlighter(HighlightLines<'static>);

impl Highlighter {
    pub fn new(extension: &str) -> Option<Self> {
        let syntax = SYNTAXES.find_syntax_by_extension(extension)?;
        Some(Self(HighlightLines::new(syntax, &THEME)))
    }

    pub fn line(&mut self, line: &str) -> Option<Vec<(usize, HighlightStyle)>> {
        let mut runs: Vec<(usize, HighlightStyle)> = Vec::new();
        for (style, text) in self.0.highlight_line(line, &SYNTAXES).ok()? {
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
        Some(runs)
    }
}

pub fn highlight(source: &str, extension: &str) -> Option<Vec<(usize, HighlightStyle)>> {
    let mut highlighter = Highlighter::new(extension)?;
    let mut runs: Vec<(usize, HighlightStyle)> = Vec::new();
    for line in LinesWithEndings::from(source) {
        for (length, highlight) in highlighter.line(line)? {
            if let Some((len, previous)) = runs.last_mut()
                && *previous == highlight
            {
                *len += length;
            } else {
                runs.push((length, highlight));
            }
        }
    }
    Some(runs)
}
