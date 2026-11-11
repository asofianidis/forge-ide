//! UI and syntax themes.
//!
//! A [`Theme`] describes both the egui chrome (panels, widgets, selection)
//! and the syntax-highlighting palette. Two themes are built in (`Blossom`,
//! the default, and `Blossom Light`), and any number of custom themes can be
//! loaded from TOML files in the user's themes directory (see
//! [`themes_dir`]).
//!
//! Custom theme files only need to specify the values they change; every
//! missing value is inherited from the theme named by `extends`
//! (default: `Blossom`). See `themes/example.toml` in the repository.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context as _, Result, bail};
use egui::{Color32, CornerRadius, Stroke};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const DEFAULT_THEME: &str = "Blossom";

/// An sRGB color, written as `"#rrggbb"` or `"#rrggbbaa"` in theme files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub Color32);

impl Color {
    const fn hex(rgb: u32) -> Self {
        Self(Color32::from_rgb(
            (rgb >> 16) as u8,
            (rgb >> 8) as u8,
            rgb as u8,
        ))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b, a] = self.0.to_srgba_unmultiplied();
        if a == 255 {
            write!(f, "#{r:02x}{g:02x}{b:02x}")
        } else {
            write!(f, "#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }
}

impl FromStr for Color {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        let byte = |i: usize| {
            hex.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| {
                    format!("invalid color {s:?}, expected \"#rrggbb\" or \"#rrggbbaa\"")
                })
        };
        match hex.len() {
            6 => Ok(Self(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?))),
            8 => Ok(Self(Color32::from_rgba_unmultiplied(
                byte(0)?,
                byte(2)?,
                byte(4)?,
                byte(6)?,
            ))),
            _ => Err(format!(
                "invalid color {s:?}, expected \"#rrggbb\" or \"#rrggbbaa\""
            )),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Colors for the application chrome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiColors {
    /// Main text color.
    pub text: Color,
    /// Secondary text (labels, status bar, inactive tabs).
    pub text_muted: Color,
    /// Side panels, menu bar and status bar.
    pub panel: Color,
    /// Editor surface.
    pub editor: Color,
    /// Popups, menus and windows.
    pub surface: Color,
    /// Resting background of buttons and other widgets.
    pub widget: Color,
    /// Widget background while hovered.
    pub widget_hover: Color,
    /// Accent for primary actions, the active tab and focus rings.
    pub accent: Color,
    /// Text drawn on top of `accent`.
    pub accent_text: Color,
    /// Text selection background.
    pub selection: Color,
    /// Separators and widget outlines.
    pub border: Color,
    pub cursor: Color,
    pub link: Color,
    pub warning: Color,
    pub error: Color,
}

/// Colors for syntax highlighting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyntaxColors {
    pub text: Color,
    pub comment: Color,
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub constant: Color,
    pub function: Color,
    #[serde(rename = "type")]
    pub type_: Color,
    pub variable: Color,
    pub operator: Color,
    pub punctuation: Color,
    pub tag: Color,
    pub attribute: Color,
    pub invalid: Color,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Whether egui should use its dark-mode defaults underneath.
    pub dark: bool,
    /// Corner radius of buttons, tabs and popups, in points.
    pub corner_radius: u8,
    pub ui: UiColors,
    pub syntax: SyntaxColors,
}

impl Theme {
    /// The default theme: a deep plum dark theme with a magenta accent
    /// and soft, rounded widgets.
    pub fn blossom() -> Self {
        Self {
            name: "Blossom".to_owned(),
            dark: true,
            corner_radius: 8,
            ui: UiColors {
                text: Color::hex(0xf2e6ef),
                text_muted: Color::hex(0xa8939f),
                panel: Color::hex(0x1b1620),
                editor: Color::hex(0x221d27),
                surface: Color::hex(0x2a2330),
                widget: Color::hex(0x2c2532),
                widget_hover: Color::hex(0x3a2f40),
                accent: Color::hex(0xa3004c),
                accent_text: Color::hex(0xfbe8f1),
                selection: Color::hex(0x6b2a4f),
                border: Color::hex(0x342b38),
                cursor: Color::hex(0xf472b6),
                link: Color::hex(0xf9a8d4),
                warning: Color::hex(0xf5c46b),
                error: Color::hex(0xf87171),
            },
            syntax: SyntaxColors {
                text: Color::hex(0xe9dde6),
                comment: Color::hex(0x7c6a78),
                keyword: Color::hex(0xf472b6),
                string: Color::hex(0xa5d6a7),
                number: Color::hex(0xf9b38a),
                constant: Color::hex(0xf9b38a),
                function: Color::hex(0xc4a7f5),
                type_: Color::hex(0x8fd3e8),
                variable: Color::hex(0xe9dde6),
                operator: Color::hex(0xd6a0c4),
                punctuation: Color::hex(0xa8939f),
                tag: Color::hex(0xf472b6),
                attribute: Color::hex(0xc4a7f5),
                invalid: Color::hex(0xf87171),
            },
        }
    }

    /// Light counterpart of [`Theme::blossom`]: soft pink surfaces with a
    /// raspberry accent.
    pub fn blossom_light() -> Self {
        Self {
            name: "Blossom Light".to_owned(),
            dark: false,
            corner_radius: 8,
            ui: UiColors {
                text: Color::hex(0x4a1d3f),
                text_muted: Color::hex(0x8a6380),
                panel: Color::hex(0xf2e1ee),
                editor: Color::hex(0xfaf4f8),
                surface: Color::hex(0xfdf8fb),
                widget: Color::hex(0xecd6e6),
                widget_hover: Color::hex(0xe3c5db),
                accent: Color::hex(0xaa3067),
                accent_text: Color::hex(0xffffff),
                selection: Color::hex(0xf2bcd8),
                border: Color::hex(0xe2c8da),
                cursor: Color::hex(0xaa3067),
                link: Color::hex(0xaa3067),
                warning: Color::hex(0xb7791f),
                error: Color::hex(0xc53030),
            },
            syntax: SyntaxColors {
                text: Color::hex(0x3d2236),
                comment: Color::hex(0x9c8496),
                keyword: Color::hex(0xb5306f),
                string: Color::hex(0x2f7d4a),
                number: Color::hex(0xb35a1f),
                constant: Color::hex(0xb35a1f),
                function: Color::hex(0x6b3fc1),
                type_: Color::hex(0x1f6f8b),
                variable: Color::hex(0x3d2236),
                operator: Color::hex(0x8a4b78),
                punctuation: Color::hex(0x8a6380),
                tag: Color::hex(0xb5306f),
                attribute: Color::hex(0x6b3fc1),
                invalid: Color::hex(0xc53030),
            },
        }
    }

    pub fn builtins() -> Vec<Self> {
        vec![Self::blossom(), Self::blossom_light()]
    }

    /// Parses a theme file, filling unspecified values from the theme it
    /// `extends` (looked up in `bases`, default [`DEFAULT_THEME`]).
    pub fn from_toml(src: &str, bases: &[Theme]) -> Result<Self> {
        let mut overlay: toml::Table = toml::from_str(src)?;
        let extends = match overlay.remove("extends") {
            Some(toml::Value::String(s)) => s,
            Some(_) => bail!("`extends` must be a string"),
            None => DEFAULT_THEME.to_owned(),
        };
        let Some(base) = bases.iter().find(|t| t.name == extends) else {
            bail!("`extends` refers to unknown theme {extends:?}");
        };
        if !overlay.contains_key("name") {
            bail!("missing `name`");
        }
        let mut table = toml::Table::try_from(base)?;
        merge(&mut table, overlay);
        Ok(table.try_into()?)
    }

    /// Applies the chrome colors to an egui context.
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_visuals(self.visuals());
    }

    pub fn visuals(&self) -> egui::Visuals {
        let c = &self.ui;
        let radius = CornerRadius::same(self.corner_radius);
        let mut v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        v.override_text_color = None;
        v.hyperlink_color = c.link.0;
        v.panel_fill = c.panel.0;
        v.window_fill = c.surface.0;
        v.window_stroke = Stroke::new(1.0, c.border.0);
        v.window_corner_radius = CornerRadius::same(self.corner_radius.saturating_add(4));
        v.menu_corner_radius = radius;
        v.extreme_bg_color = c.editor.0;
        v.text_edit_bg_color = Some(c.editor.0);
        v.code_bg_color = c.widget.0;
        v.faint_bg_color = c.editor.0;
        v.warn_fg_color = c.warning.0;
        v.error_fg_color = c.error.0;
        v.selection.bg_fill = c.selection.0;
        v.selection.stroke = Stroke::new(1.0, c.text.0);
        v.text_cursor.stroke = Stroke::new(2.0, c.cursor.0);

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = c.panel.0;
        w.noninteractive.weak_bg_fill = c.panel.0;
        w.noninteractive.bg_stroke = Stroke::new(1.0, c.border.0);
        w.noninteractive.fg_stroke = Stroke::new(1.0, c.text_muted.0);
        w.noninteractive.corner_radius = radius;

        w.inactive.bg_fill = c.widget.0;
        w.inactive.weak_bg_fill = c.widget.0;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, c.text.0);
        w.inactive.corner_radius = radius;

        w.hovered.bg_fill = c.widget_hover.0;
        w.hovered.weak_bg_fill = c.widget_hover.0;
        w.hovered.bg_stroke = Stroke::new(1.0, c.border.0);
        w.hovered.fg_stroke = Stroke::new(1.5, c.text.0);
        w.hovered.corner_radius = radius;

        w.active.bg_fill = c.accent.0;
        w.active.weak_bg_fill = c.accent.0;
        w.active.bg_stroke = Stroke::new(1.0, c.accent.0);
        w.active.fg_stroke = Stroke::new(2.0, c.accent_text.0);
        w.active.corner_radius = radius;

        w.open.bg_fill = c.widget_hover.0;
        w.open.weak_bg_fill = c.widget_hover.0;
        w.open.bg_stroke = Stroke::new(1.0, c.border.0);
        w.open.fg_stroke = Stroke::new(1.0, c.text.0);
        w.open.corner_radius = radius;

        v
    }

    /// Builds a syntect theme from the syntax palette.
    pub fn syntect_theme(&self) -> syntect::highlighting::Theme {
        use syntect::highlighting::{
            Color as SColor, FontStyle, ScopeSelectors, StyleModifier, ThemeItem, ThemeSettings,
        };

        let sc = |c: Color| {
            let [r, g, b, a] = c.0.to_srgba_unmultiplied();
            SColor { r, g, b, a }
        };
        let s = &self.syntax;
        let rules: [(&str, Color, FontStyle); 14] = [
            (
                "comment, punctuation.definition.comment",
                s.comment,
                FontStyle::ITALIC,
            ),
            (
                "keyword, storage, keyword.operator.word, keyword.control",
                s.keyword,
                FontStyle::empty(),
            ),
            (
                "string, string.quoted, constant.character",
                s.string,
                FontStyle::empty(),
            ),
            ("constant.numeric", s.number, FontStyle::empty()),
            (
                "constant.language, constant.other, support.constant, variable.language",
                s.constant,
                FontStyle::empty(),
            ),
            (
                "entity.name.function, support.function, meta.function-call variable.function, variable.function",
                s.function,
                FontStyle::empty(),
            ),
            (
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, support.type, support.class, storage.type.primitive, entity.other.inherited-class",
                s.type_,
                FontStyle::empty(),
            ),
            (
                "variable, variable.parameter, meta.definition.variable",
                s.variable,
                FontStyle::empty(),
            ),
            ("keyword.operator", s.operator, FontStyle::empty()),
            (
                "punctuation, meta.brace, meta.delimiter",
                s.punctuation,
                FontStyle::empty(),
            ),
            ("entity.name.tag, meta.tag", s.tag, FontStyle::empty()),
            (
                "entity.other.attribute-name",
                s.attribute,
                FontStyle::empty(),
            ),
            ("markup.heading, markup.bold", s.keyword, FontStyle::BOLD),
            ("invalid", s.invalid, FontStyle::UNDERLINE),
        ];

        syntect::highlighting::Theme {
            name: Some(self.name.clone()),
            settings: ThemeSettings {
                foreground: Some(sc(s.text)),
                background: Some(sc(self.ui.editor)),
                caret: Some(sc(self.ui.cursor)),
                selection: Some(sc(self.ui.selection)),
                ..Default::default()
            },
            scopes: rules
                .into_iter()
                .map(|(scope, color, font_style)| ThemeItem {
                    scope: ScopeSelectors::from_str(scope).expect("built-in scope selector"),
                    style: StyleModifier {
                        foreground: Some(sc(color)),
                        background: None,
                        font_style: Some(font_style),
                    },
                })
                .collect(),
            ..Default::default()
        }
    }
}

/// Recursively overlays `overlay` onto `base`.
fn merge(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(key, v);
            }
        }
    }
}

/// `<config dir>/forge-ide`, e.g. `~/.config/forge-ide` on Linux or
/// `%APPDATA%\forge-ide` on Windows.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("forge-ide"))
}

/// Directory scanned for custom `*.toml` theme files.
pub fn themes_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("themes"))
}

/// All available themes: built-ins first, then custom themes from `dir`
/// sorted by name. Files that fail to load are reported in the returned
/// error list instead of aborting.
pub fn load_all(dir: Option<&Path>) -> (Vec<Theme>, Vec<String>) {
    let mut themes = Theme::builtins();
    let mut errors = Vec::new();
    let Some(dir) = dir else {
        return (themes, errors);
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (themes, errors);
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();

    let mut custom = Vec::new();
    for path in paths {
        let result = std::fs::read_to_string(&path)
            .with_context(|| "could not read file".to_owned())
            .and_then(|src| Theme::from_toml(&src, &themes));
        match result {
            Ok(theme) => {
                // A custom theme with a built-in's name replaces it.
                if let Some(existing) = themes.iter_mut().find(|t| t.name == theme.name) {
                    *existing = theme;
                } else {
                    custom.push(theme);
                }
            }
            Err(e) => errors.push(format!("{}: {e:#}", path.display())),
        }
    }
    custom.sort_by(|a, b| a.name.cmp(&b.name));
    themes.extend(custom);
    (themes, errors)
}

/// Persisted user preferences.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    pub theme: Option<String>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("settings.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path().context("no config directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_round_trip() {
        for s in ["#a3004c", "#ff000080"] {
            assert_eq!(s.parse::<Color>().unwrap().to_string(), s);
        }
        assert!("#12345".parse::<Color>().is_err());
        assert!("#zzzzzz".parse::<Color>().is_err());
    }

    #[test]
    fn builtins_round_trip_through_toml() {
        for theme in Theme::builtins() {
            let src = toml::to_string(&theme).unwrap();
            assert_eq!(Theme::from_toml(&src, &Theme::builtins()).unwrap(), theme);
        }
    }

    #[test]
    fn partial_theme_inherits_from_base() {
        let src = r##"
            name = "Mine"
            extends = "Blossom Light"
            [ui]
            accent = "#ff0000"
        "##;
        let theme = Theme::from_toml(src, &Theme::builtins()).unwrap();
        let base = Theme::blossom_light();
        assert_eq!(theme.name, "Mine");
        assert!(!theme.dark);
        assert_eq!(theme.ui.accent, Color::hex(0xff0000));
        assert_eq!(theme.ui.panel, base.ui.panel);
        assert_eq!(theme.syntax, base.syntax);
    }

    #[test]
    fn invalid_themes_are_rejected() {
        let bases = Theme::builtins();
        assert!(Theme::from_toml("[ui]\naccent = \"#ff0000\"", &bases).is_err());
        assert!(Theme::from_toml("name = \"x\"\nextends = \"Nope\"", &bases).is_err());
        assert!(Theme::from_toml("name = \"x\"\n[ui]\naccent = \"red\"", &bases).is_err());
    }

    #[test]
    fn example_theme_parses() {
        let src = include_str!("../themes/example.toml");
        Theme::from_toml(src, &Theme::builtins()).unwrap();
    }

    #[test]
    fn syntect_theme_builds() {
        for theme in Theme::builtins() {
            assert!(!theme.syntect_theme().scopes.is_empty());
        }
    }
}
