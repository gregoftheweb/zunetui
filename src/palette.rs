use ratatui::style::Color;
use std::{
    env, fs,
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub primary: Color,
    pub accent: Color,
    pub muted: Color,
    pub background: Color,
    pub success: Color,
    pub error: Color,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    TronBlue,
    CherryRed,
    Orange,
    Green,
    System,
}

impl Theme {
    pub const ALL: [Self; 5] = [
        Self::TronBlue,
        Self::CherryRed,
        Self::Orange,
        Self::Green,
        Self::System,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::TronBlue => "Tron Blue",
            Self::CherryRed => "Cherry Red",
            Self::Orange => "Orange",
            Self::Green => "Green",
            Self::System => "System",
        }
    }
    pub const fn key(self) -> &'static str {
        match self {
            Self::TronBlue => "tron-blue",
            Self::CherryRed => "cherry-red",
            Self::Orange => "orange",
            Self::Green => "green",
            Self::System => "system",
        }
    }
    pub fn from_key(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == value)
    }
}

const TRON_BLUE: Palette = Palette {
    primary: Color::Rgb(63, 210, 255),
    accent: Color::Rgb(0, 140, 210),
    muted: Color::Rgb(110, 145, 160),
    background: Color::Rgb(3, 12, 18),
    success: Color::Rgb(80, 235, 220),
    error: Color::Rgb(255, 105, 125),
};
const CHERRY_RED: Palette = Palette {
    primary: Color::Rgb(255, 92, 112),
    accent: Color::Rgb(205, 35, 65),
    muted: Color::Rgb(155, 105, 115),
    background: Color::Rgb(20, 4, 8),
    success: Color::Rgb(255, 160, 175),
    error: Color::Rgb(255, 205, 70),
};
const ORANGE: Palette = Palette {
    primary: Color::Rgb(255, 155, 45),
    accent: Color::Rgb(220, 95, 0),
    muted: Color::Rgb(160, 125, 90),
    background: Color::Rgb(20, 9, 2),
    success: Color::Rgb(255, 205, 90),
    error: Color::Rgb(255, 85, 65),
};
const GREEN: Palette = Palette {
    primary: Color::Rgb(60, 230, 125),
    accent: Color::Rgb(0, 160, 75),
    muted: Color::Rgb(95, 150, 115),
    background: Color::Rgb(2, 16, 8),
    success: Color::Rgb(120, 255, 175),
    error: Color::Rgb(255, 100, 115),
};
static CURRENT: OnceLock<RwLock<Palette>> = OnceLock::new();

pub fn palette() -> Palette {
    *CURRENT
        .get_or_init(|| RwLock::new(TRON_BLUE))
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub fn apply(theme: Theme) {
    let selected = match theme {
        Theme::TronBlue => TRON_BLUE,
        Theme::CherryRed => CHERRY_RED,
        Theme::Orange => ORANGE,
        Theme::Green => GREEN,
        Theme::System => system_palette().unwrap_or(TRON_BLUE),
    };
    *CURRENT
        .get_or_init(|| RwLock::new(TRON_BLUE))
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = selected;
}

fn system_palette() -> Option<Palette> {
    let path = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?
        .join("omarchy/current/theme/colors.toml");
    let contents = fs::read_to_string(path).ok()?;
    let color = |key: &str| parse_color(value(&contents, key)?);
    Some(Palette {
        primary: color("foreground").or_else(|| color("accent"))?,
        accent: color("accent").or_else(|| color("blue"))?,
        muted: color("muted").or_else(|| color("dark_foreground"))?,
        background: color("background")?,
        success: color("green").or_else(|| color("bright_green"))?,
        error: color("red").or_else(|| color("bright_red"))?,
    })
}
fn value<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate.trim() == key).then(|| value.trim().trim_matches(['"', '\'']))
    })
}
fn parse_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6).then_some(Color::Rgb(
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_hex_colors() {
        assert_eq!(parse_color("#22E6FF"), Some(Color::Rgb(34, 230, 255)));
        assert_eq!(parse_color("bad"), None);
    }
}
