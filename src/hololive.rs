//! Bundled Ccryu pixel art, converted by tools/gen_hololive.py.

use serde::Deserialize;

pub const SHEETS: &[(&str, &[u8])] = &[
    ("ame", include_bytes!("../assets/hololive/ame.png")),
    ("baelz", include_bytes!("../assets/hololive/baelz.png")),
    ("calli", include_bytes!("../assets/hololive/calli.png")),
    ("fauna", include_bytes!("../assets/hololive/fauna.png")),
    ("gura", include_bytes!("../assets/hololive/gura.png")),
    ("ina", include_bytes!("../assets/hololive/ina.png")),
    ("irys", include_bytes!("../assets/hololive/irys.png")),
    ("kiara", include_bytes!("../assets/hololive/kiara.png")),
    ("kronii", include_bytes!("../assets/hololive/kronii.png")),
    ("lamy", include_bytes!("../assets/hololive/lamy.png")),
    (
        "momosuzu",
        include_bytes!("../assets/hololive/momosuzu.png"),
    ),
    ("mumei", include_bytes!("../assets/hololive/mumei.png")),
    ("omaru", include_bytes!("../assets/hololive/omaru.png")),
    ("sana", include_bytes!("../assets/hololive/sana.png")),
    (
        "shishiro",
        include_bytes!("../assets/hololive/shishiro.png"),
    ),
];

#[derive(Deserialize)]
pub struct Metadata {
    pub name: String,
    pub frame_durations_ms: Vec<u64>,
}

pub fn metadata() -> Vec<Metadata> {
    serde_json::from_str(include_str!("../assets/hololive/timing.json"))
        .expect("bundled Hololive timing metadata")
}
