mod config;
mod core;
mod daemon;
mod install;
mod log;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod overlay;
#[cfg(target_os = "linux")]
#[path = "overlay_wayland.rs"]
mod overlay;
mod paths;
mod protocol;
mod render;
mod shim;
mod sound;
mod stack;
mod text;
#[cfg(target_os = "linux")]
mod timeline;
#[cfg(target_os = "linux")]
mod feed;
#[cfg(target_os = "linux")]
mod overlay_x11;

use protocol::{Kind, Msg};

fn usage() -> ! {
    eprintln!(
        "usage: clawdtoast <command>\n\
         \x20 hook EVENT        run as a Claude Code hook (reads the hook JSON on stdin)\n\
         \x20 daemon [--headless]\n\
         \x20 test              show one card of each kind\n\
         \x20 reload            make the running daemon re-read config.toml\n\
         \x20 install [--force] add clawdtoast's hooks to ~/.claude/settings.json\n\
         \x20 uninstall         remove them again"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["hook", event] => shim::run(event), // always exits 0
        ["daemon"] => daemon::run(false),
        ["daemon", "--headless"] => daemon::run(true),
        ["overlay"] => overlay::run(),
        ["test"] => self_test(),
        ["reload"] => {
            if shim::send(&Msg::Reload).is_err() {
                println!("daemon is not running; config is read when it starts");
            }
        }
        ["install"] => finish(install::run(true, false)),
        ["install", "--force"] => finish(install::run(true, true)),
        ["uninstall"] => finish(install::run(false, false)),
        _ => usage(),
    }
}

fn finish(r: Result<(), String>) {
    if let Err(e) = r {
        eprintln!("clawdtoast: {e}");
        std::process::exit(1);
    }
}

/// Shows one card of each kind so placement and sound can be checked by eye and ear.
fn self_test() {
    if let Err(e) = shim::ensure_daemon() {
        eprintln!("could not start daemon: {e}");
        std::process::exit(1);
    }
    let cards = [
        (Kind::Done, "Wrote design doc for overlay window and hook shim"),
        (Kind::Permission, "Claude needs your permission to use Bash"),
        (Kind::Idle, "Claude is waiting for your input"),
    ];
    for (i, (kind, text)) in cards.into_iter().enumerate() {
        let m = Msg::Alert {
            // Distinct sessions, so the cards stack instead of replacing each other.
            session_id: format!("test{i}"),
            kind,
            project: "clawdtoast".into(),
            text: text.into(),
            ts: protocol::now_ms(),
        };
        let _ = shim::send(&m);
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
    // Close the fake sessions so the daemon can exit once the cards are gone.
    for i in 0..3 {
        let _ = shim::send(&Msg::SessionClose { session_id: format!("test{i}") });
    }
}
