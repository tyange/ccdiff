use std::{env, path::Path};

use ratatui::style::Color;
use syntect::{
    easy::HighlightLines,
    highlighting::Theme,
    parsing::{SyntaxReference, SyntaxSet},
};
use two_face::theme::EmbeddedThemeName;

/// 한 줄을 색이 붙은 조각들로 나눈 것.
pub type Spans = Vec<(Color, String)>;

const DARK_THEMES: &[(&str, EmbeddedThemeName)] = &[
    ("Catppuccin Mocha", EmbeddedThemeName::CatppuccinMocha),
    ("Dracula", EmbeddedThemeName::Dracula),
    ("TwoDark", EmbeddedThemeName::TwoDark),
    ("Monokai", EmbeddedThemeName::MonokaiExtended),
    ("Gruvbox Dark", EmbeddedThemeName::GruvboxDark),
    ("Nord", EmbeddedThemeName::Nord),
];

const LIGHT_THEMES: &[(&str, EmbeddedThemeName)] = &[
    ("GitHub", EmbeddedThemeName::Github),
    ("Catppuccin Latte", EmbeddedThemeName::CatppuccinLatte),
    ("One Half Light", EmbeddedThemeName::OneHalfLight),
    ("Solarized Light", EmbeddedThemeName::SolarizedLight),
    ("Gruvbox Light", EmbeddedThemeName::GruvboxLight),
];

/// Ghostty 테마가 macOS 외관을 따르므로 같은 값을 본다. CCDIFF_BG=light|dark 가 있으면 그게 우선.
fn system_is_light() -> bool {
    match env::var("CCDIFF_BG").as_deref() {
        Ok("light") => return true,
        Ok("dark") => return false,
        _ => {}
    }
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).contains("Dark"))
        .unwrap_or(false)
}

pub struct Highlighter {
    syntaxes: SyntaxSet,
    dark: Vec<Theme>,
    light_themes: Vec<Theme>,
    pub light: bool,
    system_light: bool,
    cur_dark: usize,
    cur_light: usize,
}

impl Highlighter {
    pub fn new() -> Self {
        let set = two_face::theme::extra();
        let load = |list: &[(&str, EmbeddedThemeName)]| list.iter().map(|(_, t)| set.get(*t).clone()).collect();
        let light = system_is_light();
        Self {
            syntaxes: two_face::syntax::extra_newlines(),
            dark: load(DARK_THEMES),
            light_themes: load(LIGHT_THEMES),
            light,
            system_light: light,
            cur_dark: 0,
            cur_light: 0,
        }
    }

    /// 지금 고른 테마: (라이트인가, 목록 안 번호). 백그라운드 하이라이트에 넘긴다.
    pub fn selection(&self) -> (bool, usize) {
        if self.light { (true, self.cur_light) } else { (false, self.cur_dark) }
    }

    fn theme_of(&self, (light, i): (bool, usize)) -> &Theme {
        if light { &self.light_themes[i] } else { &self.dark[i] }
    }

    pub fn theme_name(&self) -> &'static str {
        if self.light { LIGHT_THEMES[self.cur_light].0 } else { DARK_THEMES[self.cur_dark].0 }
    }

    pub fn next_theme(&mut self) {
        if self.light {
            self.cur_light = (self.cur_light + 1) % self.light_themes.len();
        } else {
            self.cur_dark = (self.cur_dark + 1) % self.dark.len();
        }
    }

    /// 바뀌었으면 true — 호출부가 렌더 캐시를 비운다.
    pub fn sync_background(&mut self) -> bool {
        // 시스템 외관이 바뀐 순간에만 따라간다 — b 로 직접 고른 값은 그대로 둔다.
        let now = system_is_light();
        if now == self.system_light {
            return false;
        }
        self.system_light = now;
        self.light = now;
        true
    }

    /// (추가 줄 배경, 삭제 줄 배경)
    pub fn diff_bgs(&self) -> (Color, Color) {
        if self.light {
            (Color::Rgb(218, 251, 225), Color::Rgb(255, 228, 226))
        } else {
            (Color::Rgb(30, 66, 44), Color::Rgb(80, 34, 40))
        }
    }

    fn syntax_for(&self, path: &Path) -> &SyntaxReference {
        path.extension()
            .and_then(|x| x.to_str())
            .and_then(|x| self.syntaxes.find_syntax_by_extension(x))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    /// 줄들을 처음부터 이어서 칠한다 — 여러 줄 주석·문자열 상태가 줄 사이로 이어진다.
    pub fn highlight(&self, path: &Path, lines: &[String]) -> Vec<Spans> {
        self.highlight_as(path, lines, self.selection())
    }

    pub fn highlight_as(&self, path: &Path, lines: &[String], sel: (bool, usize)) -> Vec<Spans> {
        let mut h = HighlightLines::new(self.syntax_for(path), self.theme_of(sel));
        lines
            .iter()
            .map(|l| {
                let with_nl = format!("{l}\n");
                match h.highlight_line(&with_nl, &self.syntaxes) {
                    Ok(parts) => parts
                        .into_iter()
                        .map(|(st, t)| {
                            let c = st.foreground;
                            (Color::Rgb(c.r, c.g, c.b), t.trim_end_matches('\n').to_string())
                        })
                        .filter(|(_, t)| !t.is_empty())
                        .collect(),
                    Err(_) => vec![(Color::Reset, l.clone())],
                }
            })
            .collect()
    }
}
