//! `~/.config/clawdtoast/config.toml`. Every key is optional; unknown keys are ignored.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Position {
    #[cfg(target_os = "linux")]
    pub fn is_top(self) -> bool {
        matches!(self, Position::TopLeft | Position::TopRight)
    }

    #[cfg(target_os = "linux")]
    pub fn is_left(self) -> bool {
        matches!(self, Position::TopLeft | Position::BottomLeft)
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Sound {
    pub enabled: bool,
    pub volume: f32,
    /// WAV file played for Done cards instead of the built-in chime.
    pub done: Option<String>,
    /// WAV file played for permission/idle cards instead of the built-in ping.
    pub attention: Option<String>,
}

impl Default for Sound {
    fn default() -> Sound {
        Sound { enabled: true, volume: 0.6, done: None, attention: None }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Events {
    pub stop: bool,
    pub permission_prompt: bool,
    pub idle_prompt: bool,
    pub subagent_stop: bool,
}

impl Default for Events {
    fn default() -> Events {
        Events { stop: true, permission_prompt: true, idle_prompt: true, subagent_stop: false }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub duration_secs: f64,
    pub position: Position,
    pub max_stack: usize,
    pub sound: Sound,
    pub events: Events,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            duration_secs: 5.0,
            position: Position::default(),
            max_stack: 3,
            sound: Sound::default(),
            events: Events::default(),
        }
    }
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("CLAWDTOAST_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/clawdtoast/config.toml")
}

pub fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

impl Config {
    pub fn parse(src: &str) -> Result<Config, toml::de::Error> {
        let mut c: Config = toml::from_str(src)?;
        c.max_stack = c.max_stack.clamp(1, 6);
        if !(c.duration_secs.is_finite() && c.duration_secs > 0.0) {
            c.duration_secs = 5.0;
        }
        c.sound.volume = c.sound.volume.clamp(0.0, 2.0);
        Ok(c)
    }

    /// Reads the config file (defaults if missing or invalid), then applies env overrides
    /// that tests and quick experiments use.
    pub fn load() -> Config {
        let mut c = match std::fs::read_to_string(path()) {
            Ok(src) => Config::parse(&src).unwrap_or_else(|e| {
                crate::log::line(&format!("config: {e}; using defaults"));
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        if let Some(d) = std::env::var("CLAWDTOAST_DURATION_SECS").ok().and_then(|v| v.parse().ok()) {
            c.duration_secs = d;
        }
        if std::env::var_os("CLAWDTOAST_MUTE").is_some() {
            c.sound.enabled = false;
        }
        if std::env::var_os("CLAWDTOAST_SUBAGENT").is_some() {
            c.events.subagent_stop = true;
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.duration_secs, 5.0);
        assert_eq!(c.max_stack, 3);
        assert!(c.events.stop && !c.events.subagent_stop);
    }

    #[test]
    fn design_doc_example() {
        let c = Config::parse(
            r#"
duration_secs = 8
position = "bottom-left"
max_stack = 2
[sound]
enabled = false
volume = 0.3
mute_in_focus_mode = true
done = "~/sounds/ding.wav"
[events]
idle_prompt = false
subagent_stop = true
"#,
        )
        .unwrap();
        assert_eq!(c.duration_secs, 8.0);
        assert_eq!(c.position, Position::BottomLeft);
        assert_eq!(c.max_stack, 2);
        assert!(!c.sound.enabled && c.sound.done.is_some());
        assert!(!c.events.idle_prompt && c.events.subagent_stop && c.events.stop);
    }

    #[test]
    fn clamps_bad_values() {
        let c = Config::parse("max_stack = 99\nduration_secs = -1").unwrap();
        assert_eq!((c.max_stack, c.duration_secs), (6, 5.0));
        assert!(Config::parse("position = \"middle\"").is_err());
    }
}
