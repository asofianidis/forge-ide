//! A small GitHub-flavored Markdown renderer for PR descriptions and
//! comments: headings, paragraphs, lists, task lists, quotes, code blocks,
//! tables (as monospace), rules, and inline code/bold/italic/links.

use egui::{Color32, RichText, Stroke, Ui};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    Heading(usize, String),
    Paragraph(String),
    /// (indent level, marker, text)
    ListItem(usize, String, String),
    Quote(String),
    Code(String),
    Table(String),
    Rule,
}

/// Renders markdown `src` into `ui`.
pub fn show(ui: &mut Ui, src: &str) {
    let blocks = parse(src);
    if blocks.is_empty() {
        ui.label(RichText::new("No description provided.").italics().weak());
        return;
    }
    for block in &blocks {
        match block {
            Block::Heading(level, text) => {
                ui.add_space(if *level <= 2 { 6.0 } else { 3.0 });
                let size = match level {
                    1 => 20.0,
                    2 => 17.0,
                    3 => 15.0,
                    _ => 13.5,
                };
                inline(ui, text, Some(size), true);
                if *level <= 2 {
                    ui.separator();
                }
            }
            Block::Paragraph(text) => {
                inline(ui, text, None, false);
                ui.add_space(4.0);
            }
            Block::ListItem(indent, marker, text) => {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(8.0 + *indent as f32 * 16.0);
                    ui.label(RichText::new(marker).weak().monospace());
                    inline_contents(ui, text, None, false);
                });
            }
            Block::Quote(text) => {
                let stroke_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 4,
                        top: 2,
                        bottom: 2,
                    })
                    .show(ui, |ui| {
                        let r = ui.max_rect();
                        ui.painter().line_segment(
                            [
                                r.left_top() - egui::vec2(8.0, 0.0),
                                r.left_bottom() - egui::vec2(8.0, 0.0),
                            ],
                            Stroke::new(3.0, stroke_color),
                        );
                        ui.visuals_mut().override_text_color = Some(ui.visuals().weak_text_color());
                        inline(ui, text, None, false);
                    });
                ui.add_space(4.0);
            }
            Block::Code(code) | Block::Table(code) => {
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .corner_radius(6)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        egui::ScrollArea::horizontal()
                            .id_salt(code.as_ptr())
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(code).monospace()).extend());
                            });
                    });
                ui.add_space(4.0);
            }
            Block::Rule => {
                ui.separator();
            }
        }
    }
}

fn inline(ui: &mut Ui, text: &str, size: Option<f32>, strong: bool) {
    ui.horizontal_wrapped(|ui| inline_contents(ui, text, size, strong));
}

fn inline_contents(ui: &mut Ui, text: &str, size: Option<f32>, strong: bool) {
    ui.spacing_mut().item_spacing.x = 0.0;
    for span in parse_inline(text) {
        let mut rt = RichText::new(&span.text);
        if let Some(s) = size {
            rt = rt.size(s);
        }
        if span.bold || strong {
            rt = rt.strong();
        }
        if span.italic {
            rt = rt.italics();
        }
        if span.strike {
            rt = rt.strikethrough();
        }
        if span.code {
            rt = rt
                .monospace()
                .background_color(ui.visuals().extreme_bg_color);
        }
        match &span.link {
            Some(url) => {
                ui.hyperlink_to(rt, url);
            }
            None => {
                ui.label(rt);
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Span {
    text: String,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
}

fn parse_inline(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut cur = Span::default();
    let (mut bold, mut italic, mut strike) = (false, false, false);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let flush = |cur: &mut Span, spans: &mut Vec<Span>| {
        if !cur.text.is_empty() {
            spans.push(std::mem::take(cur));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        // Inline code.
        if c == '`'
            && let Some(end) = chars[i + 1..].iter().position(|&x| x == '`')
        {
            flush(&mut cur, &mut spans);
            spans.push(Span {
                text: chars[i + 1..i + 1 + end].iter().collect(),
                code: true,
                ..Span::default()
            });
            i += end + 2;
            continue;
        }
        // Links and images: [text](url) / ![alt](url)
        let is_image = c == '!' && next == Some('[');
        if (c == '[' || is_image)
            && let Some(link) = parse_link(&chars[i + usize::from(is_image)..])
        {
            flush(&mut cur, &mut spans);
            let (label, url, consumed) = link;
            spans.push(Span {
                text: if is_image {
                    format!(
                        "[image: {}]",
                        if label.is_empty() { "image" } else { &label }
                    )
                } else {
                    label
                },
                bold,
                italic,
                link: Some(url),
                ..Span::default()
            });
            i += consumed + usize::from(is_image);
            continue;
        }
        if c == '*' && next == Some('*') || c == '_' && next == Some('_') {
            flush(&mut cur, &mut spans);
            bold = !bold;
            i += 2;
        } else if c == '~' && next == Some('~') {
            flush(&mut cur, &mut spans);
            strike = !strike;
            i += 2;
        } else if c == '*' && (italic || next.is_some_and(|n| !n.is_whitespace())) {
            flush(&mut cur, &mut spans);
            italic = !italic;
            i += 1;
        } else {
            if cur.text.is_empty() {
                cur.bold = bold;
                cur.italic = italic;
                cur.strike = strike;
            }
            cur.text.push(c);
            i += 1;
        }
    }
    flush(&mut cur, &mut spans);
    spans
}

/// Parses `[label](url)` at the start of `chars`; returns label, url and
/// the number of chars consumed.
fn parse_link(chars: &[char]) -> Option<(String, String, usize)> {
    if chars.first() != Some(&'[') {
        return None;
    }
    let close = chars.iter().position(|&c| c == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    let label: String = chars[1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    let url = url.split_whitespace().next().unwrap_or_default().to_owned();
    Some((label, url, end + 1))
}

/// Removes `<!-- … -->` comments (PR templates are full of them).
fn strip_html_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse(src: &str) -> Vec<Block> {
    let src = strip_html_comments(&src.replace("\r\n", "\n"));
    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut lines = src.lines().peekable();

    let flush = |para: &mut String, blocks: &mut Vec<Block>| {
        let text = para.trim();
        if !text.is_empty() {
            blocks.push(Block::Paragraph(text.to_owned()));
        }
        para.clear();
    };

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) / 2;

        if let Some(fence) = trimmed
            .strip_prefix("```")
            .or_else(|| trimmed.strip_prefix("~~~"))
        {
            let _lang = fence;
            flush(&mut para, &mut blocks);
            let mut code = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") || l.trim_start().starts_with("~~~") {
                    break;
                }
                code.push(l);
            }
            blocks.push(Block::Code(code.join("\n")));
        } else if trimmed.is_empty() {
            flush(&mut para, &mut blocks);
        } else if let Some(level) = heading_level(trimmed) {
            flush(&mut para, &mut blocks);
            let text = trimmed[level..].trim().trim_end_matches('#').trim();
            blocks.push(Block::Heading(level, text.to_owned()));
        } else if is_rule(trimmed) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Rule);
        } else if let Some((marker, text)) = list_item(trimmed) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::ListItem(indent, marker, text.to_owned()));
        } else if let Some(q) = trimmed.strip_prefix('>') {
            flush(&mut para, &mut blocks);
            let mut text = q.trim().to_owned();
            while let Some(next) = lines.peek().and_then(|l| l.trim_start().strip_prefix('>')) {
                text.push(' ');
                text.push_str(next.trim());
                lines.next();
            }
            blocks.push(Block::Quote(text));
        } else if trimmed.starts_with('|') {
            flush(&mut para, &mut blocks);
            let mut rows = vec![trimmed.to_owned()];
            while let Some(next) = lines.peek().filter(|l| l.trim_start().starts_with('|')) {
                rows.push(next.trim().to_owned());
                lines.next();
            }
            blocks.push(Block::Table(rows.join("\n")));
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(trimmed);
        }
    }
    flush(&mut para, &mut blocks);
    blocks
}

fn heading_level(line: &str) -> Option<usize> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    ((1..=6).contains(&level) && line[level..].starts_with(' ')).then_some(level)
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn list_item(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            if let Some(r) = rest.strip_prefix("[ ] ") {
                return Some(("[ ] ".to_owned(), r));
            }
            if let Some(r) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                return Some(("[x] ".to_owned(), r));
            }
            return Some(("• ".to_owned(), rest));
        }
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        return Some((format!("{}. ", &line[..digits]), &line[digits + 2..]));
    }
    None
}

/// Color for a GitHub label given as a hex string without `#`.
pub fn label_color(hex: &str) -> Color32 {
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
    };
    match (byte(0), byte(2), byte(4)) {
        (Some(r), Some(g), Some(b)) => Color32::from_rgb(r, g, b),
        _ => Color32::GRAY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks() {
        let src = "# Title\n<!-- hidden\ncomment -->\nSome *text*\ncontinues.\n\n\
                   - [x] done\n- item\n  1. nested\n\n```rust\nfn x() {}\n```\n> quote\n---\n| a | b |\n|---|---|\n";
        let b = parse(src);
        assert_eq!(b[0], Block::Heading(1, "Title".into()));
        assert_eq!(b[1], Block::Paragraph("Some *text* continues.".into()));
        assert_eq!(b[2], Block::ListItem(0, "[x] ".into(), "done".into()));
        assert_eq!(b[3], Block::ListItem(0, "• ".into(), "item".into()));
        assert_eq!(b[4], Block::ListItem(1, "1. ".into(), "nested".into()));
        assert_eq!(b[5], Block::Code("fn x() {}".into()));
        assert_eq!(b[6], Block::Quote("quote".into()));
        assert_eq!(b[7], Block::Rule);
        assert!(matches!(b[8], Block::Table(_)));
    }

    #[test]
    fn parses_inline_spans() {
        let spans = parse_inline("a **b** `c` [d](http://x) snake_case");
        assert_eq!(spans[0].text, "a ");
        assert!(spans[1].bold && spans[1].text == "b");
        assert!(spans[3].code && spans[3].text == "c");
        assert_eq!(spans[5].link.as_deref(), Some("http://x"));
        assert_eq!(spans.last().unwrap().text, " snake_case");
    }
}
