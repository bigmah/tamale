//! The page's icons: 16-unit line drawings the stylesheet strokes.

use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum I {
    Check,
    Cross,
    Chev,
    Open,
    Close,
    Spark,
    Reload,
    Expand,
    Edit,
    Download,
    Shrink,
    Chat,
    Back,
    Wave,
    Mic,
    MicOff,
    Gear,
    Hangup,
    Play,
    Up,
    Folder,
    Split,
    Code,
    Artifacts,
    Power,
    Jump,
    Film,
    Sparkles,
}

fn paths(i: I) -> &'static [&'static str] {
    match i {
        I::Check => &["m3.5 8.5 3 3 6-7"],
        I::Cross => &["m4.5 4.5 7 7M11.5 4.5l-7 7"],
        I::Chev => &["m6 3.5 4.5 4.5L6 12.5"],
        I::Open => &[
            "M9.5 2.5h4v4M13.5 2.5 7.5 8.5M12 9.5v3.25c0 .41-.34.75-.75.75h-8a.75.75 0 0 1-.75-.75v-8c0-.41.34-.75.75-.75H6.5",
        ],
        I::Close => &["m4 4 8 8M12 4l-8 8"],
        I::Spark => &["M8 1.5 9.6 6.4 14.5 8 9.6 9.6 8 14.5 6.4 9.6 1.5 8l4.9-1.6Z"],
        I::Reload => &["M13.2 6.3A5.3 5.3 0 1 0 13 10M13.5 2.8v3.7H9.8"],
        I::Expand => &["M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10"],
        I::Edit => &["M10.5 2.5 13.5 5.5 6 13H3v-3ZM9 4l3 3"],
        I::Download => &["M8 2.5v8M4.5 7.5 8 11l3.5-3.5M3 13.5h10"],
        I::Shrink => &["M6 2.5V6H2.5M13.5 6H10V2.5M10 13.5V10h3.5M2.5 10H6v3.5"],
        I::Chat => &[
            "M2.5 3.75c0-.69.56-1.25 1.25-1.25h8.5c.69 0 1.25.56 1.25 1.25v6c0 .69-.56 1.25-1.25 1.25H7l-3 2.5V11h-.25c-.69 0-1.25-.56-1.25-1.25Z",
        ],
        I::Back => &["M10 3.5 5.5 8l4.5 4.5"],
        I::Wave => &["M2.5 6.5v3M5 4.5v7M7.75 2.5v11M10.5 5v6M13.25 6.75v2.5"],
        I::Mic => &[
            "M8 1.75a2.25 2.25 0 0 1 2.25 2.25v3.5a2.25 2.25 0 0 1-4.5 0V4A2.25 2.25 0 0 1 8 1.75Z",
            "M3.25 7.75a4.75 4.75 0 0 0 9.5 0M8 12.5v2",
        ],
        I::MicOff => &[
            "M10.25 6.5V4a2.25 2.25 0 0 0-4.4-.67M5.75 6.75v1a2.25 2.25 0 0 0 3.6 1.8M3.25 7.75a4.75 4.75 0 0 0 7.9 3.55M12.75 7.75c0 .5-.08 1-.22 1.45M8 12.5v2M2.5 2.5l11 11",
        ],
        I::Gear => &[
            "M8 5.75a2.25 2.25 0 1 1 0 4.5 2.25 2.25 0 0 1 0-4.5Z",
            "M8 1.75v1.5M8 12.75v1.5M14.25 8h-1.5M3.25 8h-1.5M12.42 3.58l-1.06 1.06M4.64 11.36l-1.06 1.06M12.42 12.42l-1.06-1.06M4.64 4.64 3.58 3.58",
        ],
        I::Hangup => {
            &["M1.75 9.25c3.5-3 9-3 12.5 0l-1.5 2-2.5-1v-1.75a7 7 0 0 0-4.5 0v1.75l-2.5 1Z"]
        }
        I::Play => &["M5 3.25v9.5L12.75 8Z"],
        I::Up => &["M8 13V3M3.5 7.5 8 3l4.5 4.5"],
        I::Folder => &[
            "M1.75 3h4.1l1.4 1.5h7a.75.75 0 0 1 .75.75v7.5a.75.75 0 0 1-.75.75H1.75a.75.75 0 0 1-.75-.75v-9A.75.75 0 0 1 1.75 3Z",
        ],
        I::Split => &[
            "M3.75 2.75h8.5c.97 0 1.75.78 1.75 1.75v7c0 .97-.78 1.75-1.75 1.75h-8.5C2.78 13.25 2 12.47 2 11.5v-7c0-.97.78-1.75 1.75-1.75Z",
            "M8 2.75v10.5",
        ],
        I::Code => &[
            "m5.6 6.7 6.2-3.6 1.5 2.6-6.2 3.6ZM2.6 9.1l3.3-1.9.9 1.6-3.3 1.9ZM8.4 3.8l1.4-.8M8.8 8.4 6.3 14M8.8 8.4l2.5 5.6",
        ],
        I::Artifacts => &[
            "M3.5 2.5h9A1.5 1.5 0 0 1 14 4v8a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 12V4a1.5 1.5 0 0 1 1.5-1.5Z",
            "M2 5.5h12M5 8.5h6M5 10.75h3.5",
        ],
        I::Power => &["M8 2.25v5.5M4.4 4.6a5 5 0 1 0 7.2 0"],
        I::Jump => &["M8 3v10M3.5 8.5 8 13l4.5-4.5"],
        I::Film => &[
            "M3.5 3.25h9a1.75 1.75 0 0 1 1.75 1.75v6a1.75 1.75 0 0 1-1.75 1.75h-9A1.75 1.75 0 0 1 1.75 11V5A1.75 1.75 0 0 1 3.5 3.25Z",
            "M6.5 6v4l3.5-2z",
        ],
        I::Sparkles => &[
            "M7 2.5 8.2 5.8 11.5 7 8.2 8.2 7 11.5 5.8 8.2 2.5 7 5.8 5.8z",
            "M12 10.5l.6 1.4 1.4.6-1.4.6-.6 1.4-.6-1.4-1.4-.6 1.4-.6z",
        ],
    }
}

/// The icon `i`, as an inline SVG.
pub fn icon(i: I) -> Element {
    rsx! {
        svg { view_box: "0 0 16 16", "aria-hidden": "true",
            for d in paths(i).iter() {
                path { d: "{d}" }
            }
        }
    }
}

/// The send button's icon, which the stylesheet swaps for a stop square while the agent works.
pub fn send_icons() -> Element {
    rsx! {
        svg { class: "i-send", view_box: "0 0 16 16", "aria-hidden": "true", path { d: "M8 13V3M3.5 7.5 8 3l4.5 4.5" } }
        svg { class: "i-stop", view_box: "0 0 16 16", "aria-hidden": "true", rect { x: "4", y: "4", width: "8", height: "8", rx: "1.5" } }
    }
}

/// The chef's toque.
pub fn toque() -> Element {
    rsx! {
        svg { class: "toque", view_box: "0 0 64 64", "aria-hidden": "true",
            path { class: "toque-puff", d: "M19 41c-6.6-1.4-11.5-7.2-11.5-14 0-7.9 6.4-14.3 14.3-14.3 1.9 0 3.7.4 5.4 1.1C29.6 8.9 34.5 5.5 40.2 5.5c7.9 0 14.3 6.4 14.3 14.3 0 .6 0 1.1-.1 1.7 2.9 2 4.6 5.2 4.6 8.8 0 5.4-3.9 9.9-9 10.7" }
            path { class: "toque-body", d: "M19 41h31v9H19z" }
            path { class: "toque-pleat", d: "M27 31v10M34.5 29v12M42 31v10" }
            rect { class: "toque-band", x: "17", y: "50", width: "35", height: "8", rx: "2" }
        }
    }
}
