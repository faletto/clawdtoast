//! Which Linux display path the overlay tries, in order. Pure so it can be tested anywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// wlr-layer-shell (wlroots compositors, KDE, COSMIC, ...).
    Wayland,
    /// Override-redirect window through winit. Also used on Wayland sessions that have XWayland but
    /// no layer-shell (GNOME).
    X11,
    /// A desktop notification, for sessions where neither of the above can draw a card.
    Notify,
}

/// Backends to try, best first. `CLAWDTOAST_BACKEND=wayland|x11|notify` forces one (for testing
/// and for desktops that misreport themselves). Empty means there is no display to use.
pub fn order(env: impl Fn(&str) -> Option<String>) -> Vec<Backend> {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    match env("CLAWDTOAST_BACKEND").as_deref() {
        Some("wayland") => return vec![Backend::Wayland, Backend::Notify],
        Some("x11") => return vec![Backend::X11, Backend::Notify],
        Some("notify") => return vec![Backend::Notify],
        _ => {}
    }
    if set("WAYLAND_DISPLAY") {
        // A Wayland session without layer-shell (GNOME) still gets the Clawd card through XWayland,
        // where an override-redirect window sits above the other windows. Only a session with no X
        // server either falls back to a notification.
        if set("DISPLAY") {
            vec![Backend::Wayland, Backend::X11, Backend::Notify]
        } else {
            vec![Backend::Wayland, Backend::Notify]
        }
    } else if set("DISPLAY") {
        vec![Backend::X11, Backend::Notify]
    } else {
        vec![]
    }
}

/// True when a graphical session is reachable, so alerts have somewhere to appear.
pub fn has_display(env: impl Fn(&str) -> Option<String>) -> bool {
    !order(env).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn wayland_session_prefers_layer_shell_then_xwayland_then_notification() {
        let e = env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]);
        assert_eq!(order(e), vec![Backend::Wayland, Backend::X11, Backend::Notify]);
    }

    #[test]
    fn wayland_session_without_xwayland_falls_back_to_notification() {
        assert_eq!(order(env(&[("WAYLAND_DISPLAY", "wayland-0")])), vec![Backend::Wayland, Backend::Notify]);
        assert_eq!(
            order(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", "")])),
            vec![Backend::Wayland, Backend::Notify]
        );
    }

    #[test]
    fn x11_session_uses_the_overlay_window() {
        assert_eq!(order(env(&[("DISPLAY", ":1")])), vec![Backend::X11, Backend::Notify]);
    }

    #[test]
    fn no_display_means_nothing_to_draw_on() {
        assert!(order(env(&[("DISPLAY", "")])).is_empty());
        assert!(!has_display(env(&[])));
    }

    #[test]
    fn override_forces_a_backend() {
        assert_eq!(order(env(&[("CLAWDTOAST_BACKEND", "notify")])), vec![Backend::Notify]);
        assert_eq!(order(env(&[("CLAWDTOAST_BACKEND", "x11")])), vec![Backend::X11, Backend::Notify]);
    }
}
