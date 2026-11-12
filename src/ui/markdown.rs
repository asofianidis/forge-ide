//! A small GitHub-flavored Markdown renderer for PR descriptions, comments
//! and AI replies: headings, paragraphs, lists, task lists, quotes, code
//! blocks, tables, rules, and inline code/bold/italic/links. Common inline
//! HTML (`<b>`, `<a>`, `<br>`, `<table>`, `<details>`, …) is turned into the
//! equivalent Markdown first.

use egui::{Color32, RichText, Stroke, Ui};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    Heading(usize, String),
    Paragraph(String),
    /// (indent level, marker, text)
    ListItem(usize, String, String),
    Quote(String),
    Code(String),
    Table(Table),
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Table {
    header: Vec<String>,
    align: Vec<Align>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
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
            Block::Table(table) => {
                show_table(ui, table);
                ui.add_space(6.0);
            }
            Block::Code(code) => {
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

/// A table as a grid: bold header, a rule under it, striped rows; long
/// cells wrap, and a very wide table scrolls sideways.
fn show_table(ui: &mut Ui, table: &Table) {
    let cols = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if cols == 0 {
        return;
    }
    let border = ui.visuals().widgets.noninteractive.bg_stroke.color;
    let id = ui.next_auto_id();
    egui::Frame::new()
        .stroke(Stroke::new(1.0, border))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| {
                // Share the width out, but let short tables stay narrow.
                let max_col = (ui.available_width() / cols as f32).clamp(140.0, 420.0);
                egui::Grid::new(id.with("grid"))
                    .striped(true)
                    .spacing([16.0, 6.0])
                    .max_col_width(max_col)
                    .show(ui, |ui| {
                        let cell = |ui: &mut Ui, text: &str, col: usize, strong: bool| {
                            let align = table.align.get(col).copied().unwrap_or(Align::Left);
                            let layout = match align {
                                Align::Left => egui::Layout::left_to_right(egui::Align::Center)
                                    .with_main_wrap(true),
                                Align::Center => egui::Layout::centered_and_justified(
                                    egui::Direction::LeftToRight,
                                ),
                                Align::Right => egui::Layout::right_to_left(egui::Align::Center),
                            };
                            ui.with_layout(layout, |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                inline_contents(ui, text, None, strong);
                            });
                        };
                        for c in 0..cols {
                            let text = table.header.get(c).map_or("", String::as_str);
                            cell(ui, text, c, true);
                        }
                        ui.end_row();
                        for row in &table.rows {
                            for c in 0..cols {
                                let text = row.get(c).map_or("", String::as_str);
                                cell(ui, text, c, false);
                            }
                            ui.end_row();
                        }
                    });
            });
        });
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
    let src = html_to_markdown(&strip_html_comments(&src.replace("\r\n", "\n")));
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
        } else if trimmed.contains('|') && lines.peek().is_some_and(|l| is_table_separator(l)) {
            flush(&mut para, &mut blocks);
            let header = split_row(trimmed);
            let align = lines.next().map(table_alignment).unwrap_or_default();
            let mut rows = Vec::new();
            while let Some(next) = lines
                .peek()
                .filter(|l| l.contains('|') && !l.trim().is_empty())
            {
                rows.push(split_row(next));
                lines.next();
            }
            blocks.push(Block::Table(Table {
                header,
                align,
                rows,
            }));
        } else if trimmed.starts_with('|') {
            // Pipes without a header separator: keep the rows as written.
            flush(&mut para, &mut blocks);
            let mut rows = vec![trimmed.to_owned()];
            while let Some(next) = lines.peek().filter(|l| l.trim_start().starts_with('|')) {
                rows.push(next.trim().to_owned());
                lines.next();
            }
            blocks.push(Block::Code(rows.join("\n")));
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

/// `|---|:--:|--:|` (outer pipes optional).
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') {
        return false;
    }
    let cells = split_row(t);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim();
            let inner = c.trim_start_matches(':').trim_end_matches(':');
            !inner.is_empty() && inner.chars().all(|ch| ch == '-')
        })
}

fn table_alignment(line: &str) -> Vec<Align> {
    split_row(line)
        .iter()
        .map(|c| {
            let c = c.trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect()
}

/// The cells of a table row. `\|` and pipes inside backticks don't split.
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t
        .strip_suffix('|')
        .filter(|r| !r.ends_with('\\'))
        .unwrap_or(t);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cur).trim().to_owned()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_owned());
    cells
}

// ---- HTML ---------------------------------------------------------------

/// Turns the HTML that shows up in Markdown (and in AI replies) into
/// Markdown. Fenced code is left alone, as is anything inside backticks;
/// tags this doesn't know (e.g. `Vec<String>`) stay as written.
fn html_to_markdown(src: &str) -> String {
    if !src.contains('<') && !src.contains('&') {
        return src.to_owned();
    }
    let mut out = String::with_capacity(src.len());
    let mut in_fence = false;
    let mut html = HtmlState::default();
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence || html.in_pre {
            // Inside <pre>, keep text but still close the block.
            if html.in_pre {
                convert_line(line, &mut html, &mut out);
            } else {
                out.push_str(line);
            }
            continue;
        }
        convert_line(line, &mut html, &mut out);
    }
    out
}

#[derive(Default)]
struct HtmlState {
    in_pre: bool,
    /// Inside a `<table>`: cells in the current row, rows so far.
    table: Option<(usize, usize)>,
    /// Ordered-list counters, innermost last (`None` for `<ul>`).
    lists: Vec<Option<usize>>,
    /// Text of the open `<a href>`.
    link: Option<String>,
}

fn convert_line(line: &str, st: &mut HtmlState, out: &mut String) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_code = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' && !st.in_pre {
            in_code = !in_code;
            out.push(c);
            i += 1;
            continue;
        }
        if in_code {
            out.push(c);
            i += 1;
            continue;
        }
        if c == '&'
            && let Some((text, len)) = entity(&chars[i..])
        {
            out.push_str(text);
            i += len;
            continue;
        }
        if c == '<'
            && let Some((tag, len)) = parse_tag(&chars[i..])
            && apply_tag(&tag, st, out)
        {
            i += len;
            continue;
        }
        if st.table.is_some() && c == '\n' {
            // Rows end at </tr>; stray line breaks inside a table don't.
            i += 1;
            continue;
        }
        if st.table.is_some() && c == '|' {
            out.push_str("\\|");
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
}

fn entity(chars: &[char]) -> Option<(&'static str, usize)> {
    const ENTITIES: [(&str, &str); 9] = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&nbsp;", " "),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
    ];
    let head: String = chars.iter().take(8).collect();
    ENTITIES
        .iter()
        .find(|(e, _)| head.starts_with(e))
        .map(|(e, t)| (*t, e.chars().count()))
}

#[derive(Debug, PartialEq)]
struct Tag {
    name: String,
    closing: bool,
    attrs: String,
}

/// Tags this converts; anything else is left as text.
const KNOWN_TAGS: &[&str] = &[
    "b",
    "strong",
    "i",
    "em",
    "code",
    "kbd",
    "s",
    "del",
    "strike",
    "a",
    "br",
    "p",
    "div",
    "span",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "pre",
    "details",
    "summary",
    "img",
    "hr",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "blockquote",
    "sup",
    "sub",
    "u",
    "mark",
    "small",
    "center",
    "dl",
    "dt",
    "dd",
    "section",
    "article",
    "header",
    "footer",
    "html",
    "body",
    "head",
];

fn parse_tag(chars: &[char]) -> Option<(Tag, usize)> {
    let end = chars.iter().position(|&c| c == '>')?;
    let inner: String = chars[1..end].iter().collect();
    let inner = inner.trim();
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(r) => (true, r.trim_start()),
        None => (false, inner),
    };
    let inner = inner.strip_suffix('/').unwrap_or(inner).trim_end();
    let name_len = inner
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(inner.len());
    let name = inner[..name_len].to_ascii_lowercase();
    if !KNOWN_TAGS.contains(&name.as_str()) {
        return None;
    }
    Some((
        Tag {
            name,
            closing,
            attrs: inner[name_len..].to_owned(),
        },
        end + 1,
    ))
}

fn attr(attrs: &str, key: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let at = lower.find(&format!("{key}="))? + key.len() + 1;
    let rest = &attrs[at..];
    let (quote, rest) = match rest.chars().next()? {
        q @ ('"' | '\'') => (Some(q), &rest[1..]),
        _ => (None, rest),
    };
    let end = match quote {
        Some(q) => rest.find(q)?,
        None => rest.find(char::is_whitespace).unwrap_or(rest.len()),
    };
    Some(rest[..end].to_owned())
}

/// Writes the Markdown for `tag`. Returns false to leave the tag as text.
fn apply_tag(tag: &Tag, st: &mut HtmlState, out: &mut String) -> bool {
    let open = !tag.closing;
    match tag.name.as_str() {
        "b" | "strong" => out.push_str("**"),
        "i" | "em" => out.push('*'),
        "s" | "del" | "strike" => out.push_str("~~"),
        "code" | "kbd" if !st.in_pre => out.push('`'),
        "code" | "kbd" => {}
        "br" => out.push_str(if st.table.is_some() { " " } else { "  \n" }),
        "hr" => out.push_str("\n\n---\n\n"),
        "p" | "div" | "section" | "article" | "header" | "footer" | "center" | "blockquote"
        | "dl" => {
            if st.table.is_none() {
                out.push_str("\n\n");
            }
        }
        "dt" | "dd" => out.push('\n'),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            if open {
                let level = tag.name[1..].parse::<usize>().unwrap_or(2);
                out.push_str("\n\n");
                out.push_str(&"#".repeat(level));
                out.push(' ');
            } else {
                out.push_str("\n\n");
            }
        }
        "ul" | "ol" => {
            if open {
                st.lists.push((tag.name == "ol").then_some(0));
            } else {
                st.lists.pop();
            }
            out.push('\n');
        }
        "li" => {
            if open {
                let depth = st.lists.len().saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                match st.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        out.push_str(&format!("{n}. "));
                    }
                    _ => out.push_str("- "),
                }
            }
        }
        "pre" => {
            st.in_pre = open;
            out.push_str("\n```\n");
        }
        "details" => out.push_str("\n\n"),
        "summary" => out.push_str(if open { "\n**▸ " } else { "**\n\n" }),
        "a" => {
            if open {
                st.link = attr(&tag.attrs, "href");
                out.push('[');
            } else {
                match st.link.take() {
                    Some(href) => out.push_str(&format!("]({href})")),
                    None => out.push(']'),
                }
            }
        }
        "img" => {
            let src = attr(&tag.attrs, "src").unwrap_or_default();
            let alt = attr(&tag.attrs, "alt").unwrap_or_else(|| "image".to_owned());
            out.push_str(&format!("![{alt}]({src})"));
        }
        "table" => {
            if open {
                st.table = Some((0, 0));
                out.push_str("\n\n");
            } else {
                st.table = None;
                out.push_str("\n\n");
            }
        }
        "tr" => {
            if let Some((cells, rows)) = &mut st.table {
                if open {
                    *cells = 0;
                    out.push_str("\n|");
                } else {
                    if *rows == 0 {
                        // Markdown tables need a separator under the header.
                        out.push_str("\n|");
                        out.push_str(&"---|".repeat((*cells).max(1)));
                    }
                    *rows += 1;
                }
            }
        }
        "td" | "th" => {
            if let Some((cells, _)) = &mut st.table {
                if open {
                    out.push(' ');
                } else {
                    *cells += 1;
                    out.push_str(" |");
                }
            }
        }
        // Wrappers with no Markdown equivalent: keep their text.
        _ => {}
    }
    true
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
    fn parses_tables_with_alignment_and_inline_pipes() {
        let src = "| Area | Choice |\n|:---|---:|\n| GUI | `egui` / `a|b` |\n| Search | ripgrep \\| ignore |\n\nAfter";
        let b = parse(src);
        let Block::Table(t) = &b[0] else {
            panic!("{b:?}")
        };
        assert_eq!(t.header, ["Area", "Choice"]);
        assert_eq!(t.align, [Align::Left, Align::Right]);
        assert_eq!(t.rows[0], ["GUI", "`egui` / `a|b`"]);
        assert_eq!(t.rows[1], ["Search", "ripgrep | ignore"]);
        assert_eq!(b[1], Block::Paragraph("After".into()));
        // No outer pipes.
        let b = parse("a | b\n--|--\n1 | 2");
        assert!(matches!(&b[0], Block::Table(t) if t.rows == [["1", "2"]]));
    }

    #[test]
    fn converts_html() {
        let md = html_to_markdown(
            "<p>Hi <b>there</b> &amp; <a href=\"https://x.dev\">link</a><br>next</p>\n<ul><li>one</li><li><code>two</code></li></ul>",
        );
        assert!(md.contains("Hi **there** & [link](https://x.dev)"), "{md}");
        assert!(md.contains("- one") && md.contains("- `two`"), "{md}");

        let md = html_to_markdown(
            "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>x|y</td></tr></table>",
        );
        let b = parse(&md);
        let t = b.iter().find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        });
        let t = t.unwrap_or_else(|| panic!("{md}"));
        assert_eq!(t.header, ["A", "B"]);
        assert_eq!(t.rows[0], ["1", "x|y"]);

        // Code is left alone, and so are generics that look like tags.
        let src = "Use `<b>` and Vec<String>\n```html\n<b>raw</b>\n```\n";
        assert_eq!(html_to_markdown(src), src);

        let md = html_to_markdown("<details><summary>More</summary>\nBody</details>");
        assert!(md.contains("**▸ More**"), "{md}");
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
