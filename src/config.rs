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
        Sound {
            enabled: true,
            volume: 0.6,
            done: None,
            attention: None,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Events {
    pub stop: bool,
    pub permission_prompt: bool,
    pub idle_prompt: bool,
    pub subagent_stop: bool,
    pub blocking_questions: bool,
    pub async_questions: bool,
    pub interrupted: bool,
}

impl Default for Events {
    fn default() -> Events {
        Events {
            stop: true,
            permission_prompt: true,
            idle_prompt: true,
            subagent_stop: false,
            blocking_questions: true,
            async_questions: true,
            interrupted: false,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Attention {
    /// Retain a question card beyond the ordinary notification duration.
    pub persistent: bool,
    /// Hard lifetime, including hover; bounds stale cards when a lifecycle event is missed.
    pub max_age_secs: u64,
    /// Forget unconfirmed questions, without claiming they were answered.
    pub question_ttl_secs: u64,
    pub show_question_text: bool,
}

impl Default for Attention {
    fn default() -> Self {
        Self {
            persistent: false,
            max_age_secs: 300,
            question_ttl_secs: 3600,
            show_question_text: true,
        }
    }
}

/// Which sprite to use when animation overrides are enabled.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SpriteChoice {
    #[default]
    Auto,
    Clawd,
    Codex,
    Bae,
    Hololive,
    Custom,
}

/// Default is Clawd for Claude Code and the OpenAI spinner for Codex.
#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Animation {
    pub enabled: bool,
    pub sprite: SpriteChoice,
    pub file: Option<String>,
    /// Ignored for Hololive, which preserves each source clip's presentation times.
    pub fps: f64,
    /// Logical pixels reserved for Bae, Hololive or custom sprites (Clawd renders at 64).
    pub size: u32,
}

impl Default for Animation {
    fn default() -> Animation {
        Animation {
            enabled: false,
            sprite: SpriteChoice::Auto,
            file: None,
            fps: 8.0,
            size: 88,
        }
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
    pub animation: Animation,
    pub attention: Attention,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            duration_secs: 5.0,
            position: Position::default(),
            max_stack: 3,
            sound: Sound::default(),
            events: Events::default(),
            animation: Animation::default(),
            attention: Attention::default(),
        }
    }
}

pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("CLAWDTOAST_CONFIG") {
        return PathBuf::from(p);
    }
    crate::paths::home().join(".config/clawdtoast/config.toml")
}

pub fn expand_home(p: &str) -> PathBuf {
    let home = crate::paths::home();
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) if !home.as_os_str().is_empty() => home.join(rest),
        _ => PathBuf::from(p),
    }
}

impl Config {
    pub fn parse(src: &str) -> Result<Config, toml::de::Error> {
        let mut c: Config = toml::from_str(src)?;
        c.max_stack = c.max_stack.clamp(1, 6);
        c.attention.max_age_secs = c.attention.max_age_secs.clamp(5, 3600);
        c.attention.question_ttl_secs = c.attention.question_ttl_secs.clamp(30, 86400);
        if !(c.duration_secs.is_finite() && c.duration_secs > 0.0) {
            c.duration_secs = 5.0;
        }
        c.sound.volume = c.sound.volume.clamp(0.0, 2.0);
        if !c.animation.fps.is_finite() {
            c.animation.fps = 8.0;
        }
        c.animation.fps = c.animation.fps.clamp(1.0, 60.0);
        c.animation.size = c.animation.size.clamp(32, 88);
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
        if let Some(d) = std::env::var("CLAWDTOAST_DURATION_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
        {
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
    use super::SpriteChoice;
    use super::*;

    #[test]
    fn empty_is_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.duration_secs, 5.0);
        assert_eq!(c.max_stack, 3);
        assert!(c.events.stop && !c.events.subagent_stop);
        assert!(!c.animation.enabled);
        assert_eq!(c.animation.sprite, SpriteChoice::Auto);
        assert_eq!(c.animation.fps, 8.0);
        assert_eq!(c.animation.size, 88);
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

    #[test]
    fn attention_defaults_preserve_temporary_behavior_and_old_event_switches() {
        let c = Config::parse("[events]\nstop = true\npermission_prompt = false\nidle_prompt = false\nsubagent_stop = true").unwrap();
        assert!(!c.attention.persistent);
        assert!(c.attention.show_question_text);
        assert!(c.events.blocking_questions && c.events.async_questions && !c.events.interrupted);
        assert!(!c.events.permission_prompt && !c.events.idle_prompt && c.events.subagent_stop);
        let c = Config::parse("[attention]\npersistent=true\nmax_age_secs=0\nquestion_ttl_secs=999999\nshow_question_text=false").unwrap();
        assert_eq!(
            (c.attention.max_age_secs, c.attention.question_ttl_secs),
            (5, 86400)
        );
        assert!(!c.attention.show_question_text);
    }

    #[test]
    fn optional_animation_config_and_fps_limits() {
        let c = Config::parse(
            "[animation]\nenabled = true\nfile = '~/sprites/test.png'\nfps = 12.5\nsize = 80",
        )
        .unwrap();
        assert!(c.animation.enabled);
        assert_eq!(c.animation.file.as_deref(), Some("~/sprites/test.png"));
        assert_eq!(c.animation.fps, 12.5);
        assert_eq!(c.animation.size, 80);
        let choice = Config::parse("[animation]\nenabled = true\nsprite = 'bae'").unwrap();
        assert!(choice.animation.enabled);
        assert_eq!(choice.animation.sprite, SpriteChoice::Bae);
        assert!(Config::parse("[animation]\nsprite = 'invalid'").is_err());
        assert_eq!(
            Config::parse("[animation]\nfps = -4")
                .unwrap()
                .animation
                .fps,
            1.0
        );
        assert_eq!(
            Config::parse("[animation]\nfps = 1000")
                .unwrap()
                .animation
                .fps,
            60.0
        );
        assert_eq!(
            Config::parse("[animation]\nsize = 999")
                .unwrap()
                .animation
                .size,
            88
        );
        assert_eq!(
            Config::parse("[animation]\nsize = 4")
                .unwrap()
                .animation
                .size,
            32
        );
    }

    #[test]
    fn hololive_profile_is_opt_in() {
        let c = Config::parse("[animation]\nenabled = true\nsprite = 'hololive'").unwrap();
        assert!(c.animation.enabled);
        assert_eq!(c.animation.sprite, SpriteChoice::Hololive);
        assert_eq!(
            Config::parse("").unwrap().animation.sprite,
            SpriteChoice::Auto
        );
    }
}
