mod anim;
mod backend;
mod codex;
mod config;
mod core;
mod daemon;
mod geom;
mod hololive;
mod install;
mod ipc;
mod log;
#[cfg(target_os = "macos")]
mod macos;
mod notify;
mod overlay;
mod paths;
mod proc;
mod protocol;
mod render;
mod shim;
mod sound;
mod stack;
mod text;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

use protocol::{Kind, Msg};

fn usage() -> ! {
    eprintln!(
        "usage: clawdtoast <command>\n\
         \x20 hook EVENT        run as a Claude Code hook (reads JSON on stdin)\n\
         \x20 codex-hook EVENT  run as a Codex lifecycle hook (reads JSON on stdin)\n\
         \x20 daemon [--headless]\n\
         \x20 test [codex]      show one card of each kind\n\
         \x20 reload            make the running daemon re-read config.toml\n\
         \x20 install [--force] add clawdtoast's hooks to ~/.claude/settings.json\n\
         \x20 uninstall         remove them again\n\
         \x20 install codex     add hooks to ~/.codex/hooks.json\n\
         \x20 uninstall codex   remove clawdtoast's Codex hooks"
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["hook", event] => shim::run(event), // always exits 0
        ["codex-hook", event] => codex::run_hook(event), // always exits 0
        ["daemon"] => daemon::run(false),
        ["daemon", "--headless"] => daemon::run(true),
        ["overlay"] => overlay::run(),
        ["test"] => self_test(""),
        ["test", "codex"] => self_test("codex"),
        ["reload"] => {
            if shim::send(&Msg::Reload).is_err() {
                println!("daemon is not running; config is read when it starts");
            }
        }
        ["install"] => finish(install::run(true, false)),
        ["install", "--force"] => finish(install::run(true, true)),
        ["uninstall"] => finish(install::run(false, false)),
        ["install", "codex"] => finish(codex::install(true)),
        ["uninstall", "codex"] => finish(codex::install(false)),
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
fn self_test(agent: &str) {
    if let Err(e) = shim::ensure_daemon() {
        eprintln!("could not start daemon: {e}");
        std::process::exit(1);
    }
    let cards = [
        (
            Kind::Done,
            "Wrote design doc for overlay window and hook shim",
        ),
        (Kind::Permission, "Claude needs your permission to use Bash"),
        (Kind::Idle, "Claude is waiting for your input"),
    ];
    for (i, (kind, text)) in cards.into_iter().enumerate() {
        let m = Msg::Alert {
            // Distinct sessions, so the cards stack instead of replacing each other.
            session_id: format!("test{i}"),
            kind,
            event_id: String::new(),
            project: "clawdtoast".into(),
            text: text.into(),
            ts: protocol::now_ms(),
            pid: 0,
            app: String::new(),
            agent: agent.into(),
        };
        let _ = shim::send(&m);
        std::thread::sleep(std::time::Duration::from_millis(1200));
    }
    // Close the fake sessions so the daemon can exit once the cards are gone.
    for i in 0..3 {
        let _ = shim::send(&Msg::SessionClose {
            session_id: format!("test{i}"),
        });
    }
}
