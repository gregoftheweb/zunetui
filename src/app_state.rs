use crate::palette::Theme;
use std::{env, fs, io, path::PathBuf};
pub const CONFIG_DIR: &str = "com.columbiafoundry.ZuneTUI";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersistedState {
    pub theme: Theme,
    pub active_playlist_id: Option<u32>,
    pub active_playlist_name: Option<String>,
}

impl PersistedState {
    pub fn load() -> Self {
        fs::read_to_string(state_path())
            .ok()
            .map_or_else(Self::default, |contents| Self::parse(&contents))
    }
    pub fn save(&self) -> io::Result<()> {
        #[cfg(test)]
        return Ok(());
        #[cfg(not(test))]
        {
            let path = state_path();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, self.serialize())
        }
    }
    fn parse(contents: &str) -> Self {
        let mut state = Self::default();
        for line in contents.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "theme" => state.theme = Theme::from_key(value.trim()).unwrap_or_default(),
                "active_playlist_id" => state.active_playlist_id = value.trim().parse().ok(),
                "active_playlist_name" => state.active_playlist_name = unescape(value.trim()),
                _ => {}
            }
        }
        state
    }
    fn serialize(&self) -> String {
        format!(
            "theme={}\nactive_playlist_id={}\nactive_playlist_name={}\n",
            self.theme.key(),
            self.active_playlist_id
                .map_or_else(String::new, |id| id.to_string()),
            self.active_playlist_name
                .as_deref()
                .map(escape)
                .unwrap_or_default()
        )
    }
}
fn state_path() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR)
        .join("state")
}
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n")
}
fn unescape(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(character);
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_round_trips() {
        let state = PersistedState {
            theme: Theme::Green,
            active_playlist_id: Some(42),
            active_playlist_name: Some("Road Trip\\Mix".to_owned()),
        };
        assert_eq!(PersistedState::parse(&state.serialize()), state);
    }
}
