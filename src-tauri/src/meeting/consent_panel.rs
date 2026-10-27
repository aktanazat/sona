use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};

pub const CONSENT_PANEL_LABEL: &str = "meeting-consent";
const PANEL_WIDTH: f64 = 340.0;
const PANEL_MARGIN: f64 = 14.0;

/// The card's corner radius, which is the window's: the card fills the window,
/// so macOS clips the vibrancy view and draws the window shadow on this
/// radius. `--radius-panel` in theme.css is the same 16.
pub const PANEL_CORNER_RADIUS: f64 = 16.0;

// The heights below are the card's rendered content height at `PANEL_WIDTH`,
// measured in the shipped layout across all 24 locales with every clamped row
// at its ceiling, and recorded per row at the tallest locale, rounded up to the
// point: the window is chosen before any text is measured, and copy that does
// not fit is copy the reader loses. The prompt is the only shape whose rows
// wrap by locale, so English renders shorter than the tallest locale, which
// shows as bottom padding rather than as a band between the rows. The ritual
// cards truncate or clamp every row and measure the same in every locale.
// Change the panel's copy, type or layout and these have to be measured again.

/// The prompt's title row, its one assurance line and its buttons. Nothing
/// here is optional. The first prompt swaps the assurance for the friendlier
/// introduction, which is the same one line, so it costs no height.
const PROMPT_CONTENT: f64 = 126.0;
/// The always-record checkbox row and the gap above it. Calendar prompts only.
const PROMPT_CHECKBOX_ROW: f64 = 23.0;
/// The announce-in-chat checkbox row. Drawn with the always-record row and only
/// with it, because both are decisions the series remembers, so the two rows
/// arrive and leave together. Same shape as the row above it — one checkbox and
/// one short line — so it is the same height.
const PROMPT_ANNOUNCE_ROW: f64 = 23.0;
/// The recurring-meeting brief: one truncated line in every locale.
const PROMPT_SERIES_BRIEF: f64 = 25.0;
/// The in-session pill and the auto-record card: status row, title, buttons.
/// The title truncates and the rest is short, so every locale renders within a
/// point of the same height.
const RECORDING_CONTENT: f64 = 116.0;
/// The refused-disclosure note: a fourth row the recording card grows for when
/// the app it recorded would not take the announcement. Two lines in the
/// widest locale.
const RECORDING_DISCLOSURE_ROW: f64 = 41.0;
/// PREP's fixed rows: ritual label, title, a two-line last-time headline and
/// actions.
const PREP_CONTENT: f64 = 154.0;
/// The "My open loops" heading and the gap above it, drawn with the first row.
const PREP_LOOPS_HEADING: f64 = 28.0;
/// One verbatim open-loop row. The card shows at most two.
const PREP_LOOP_ROW: f64 = 22.0;
/// The waiting-on count.
const PREP_WAITING_ROW: f64 = 29.0;
/// Participant names, meeting counts and optional organizations, clamped to two lines.
const PREP_PARTICIPANTS_ROW: f64 = 41.0;
/// WRAP's ritual label, saved title, two-line headline and actions.
const WRAP_CONTENT: f64 = 154.0;
/// The follow-up and waiting-on delta.
const WRAP_DELTA_ROW: f64 = 29.0;

/// Which rows the panel is about to render, and therefore how tall its window
/// has to be. One window hosts both states — the same arrangement
/// `overlay::show_overlay_state` uses — so each transition resizes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsentPanelLayout {
    /// The consent prompt. Each flag is one conditional row in
    /// ConsentPanel.tsx, and the presenter has to predict them because the
    /// window is sized before the webview draws.
    Prompt {
        always_record_checkbox: bool,
        series_brief: bool,
    },
    /// Context shown before a recurring meeting begins.
    Prep {
        loop_rows: u8,
        waiting_on: bool,
        participants: bool,
    },
    /// The pill shown while this panel's meeting records. `disclosure_note` is
    /// the row it grows for when the meeting's app refused the announcement.
    Recording { disclosure_note: bool },
    /// The saved-meeting recap shown after artifact generation.
    Wrap { loop_delta: bool },
}

impl ConsentPanelLayout {
    /// Logical window height that hugs this layout's content.
    pub fn height(self) -> f64 {
        let content = match self {
            Self::Prompt {
                always_record_checkbox,
                series_brief,
            } => {
                let mut content = PROMPT_CONTENT;
                if always_record_checkbox {
                    content += PROMPT_CHECKBOX_ROW + PROMPT_ANNOUNCE_ROW;
                }
                if series_brief {
                    content += PROMPT_SERIES_BRIEF;
                }
                content
            }
            Self::Prep {
                loop_rows,
                waiting_on,
                participants,
            } => {
                let loop_rows = f64::from(loop_rows.min(2));
                PREP_CONTENT
                    + if loop_rows > 0.0 {
                        PREP_LOOPS_HEADING + PREP_LOOP_ROW * loop_rows
                    } else {
                        0.0
                    }
                    + if waiting_on { PREP_WAITING_ROW } else { 0.0 }
                    + if participants {
                        PREP_PARTICIPANTS_ROW
                    } else {
                        0.0
                    }
            }
            Self::Recording { disclosure_note } => {
                RECORDING_CONTENT
                    + if disclosure_note {
                        RECORDING_DISCLOSURE_ROW
                    } else {
                        0.0
                    }
            }
            Self::Wrap { loop_delta } => {
                WRAP_CONTENT + if loop_delta { WRAP_DELTA_ROW } else { 0.0 }
            }
        };
        // The card fills the window: macOS clips it to `PANEL_CORNER_RADIUS`
        // and draws the shadow, so there is no inset to add.
        content
    }
}

#[cfg(target_os = "macos")]
use objc2_app_kit::NSWindowSharingType;
#[cfg(target_os = "macos")]
use tauri::WebviewUrl;
#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(MeetingConsentPanel {
        config: {
            can_become_key_window: true,
            is_floating_panel: true
        }
    })
}

#[cfg(target_os = "macos")]
pub fn create(app: &AppHandle) {
    let Some((x, y)) = position(app) else {
        log::warn!("Meeting consent panel could not determine the primary monitor");
        return;
    };
    match PanelBuilder::<_, MeetingConsentPanel>::new(app, CONSENT_PANEL_LABEL)
        .url(WebviewUrl::App("/consent".into()))
        .title("Meeting recording")
        .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
        .level(PanelLevel::Status)
        .size(tauri::Size::Logical(tauri::LogicalSize {
            width: PANEL_WIDTH,
            // Only the backing size the panel is born with: every `show` sizes
            // the window for the state it is about to render.
            height: ConsentPanelLayout::Recording {
                disclosure_note: false,
            }
            .height(),
        }))
        .has_shadow(true)
        .transparent(true)
        .no_activate(false)
        .corner_radius(PANEL_CORNER_RADIUS)
        .style_mask(StyleMask::empty().borderless())
        .with_window(|window| {
            window
                .decorations(false)
                .transparent(true)
                .focusable(true)
                .accept_first_mouse(true)
                .resizable(false)
                .maximizable(false)
                .minimizable(false)
        })
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .build()
    {
        Ok(panel) => {
            // SharingNone excludes meeting titles from screen capture and
            // prevents the panel leaking into the call being recorded.
            panel.as_panel().setSharingType(NSWindowSharingType::None);
            panel.hide();
            // The window exists now, so the material can reach it: the panel is
            // built after the first `apply_window_material` runs.
            crate::shortcut::apply_consent_panel_material(app);
        }
        Err(error) => log::error!("Meeting consent panel could not be created: {error}"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn create(_app: &AppHandle) {}

/// Generation counter bumped on every `show`. The delayed hide below only
/// unmaps the window when no show happened after it was scheduled, so a
/// `HidePanel` followed in the same command batch by a `PresentPanel` for the
/// next prompt cannot take down the panel that prompt just claimed — the same
/// guard `overlay::hide_overlay_window` keeps.
static SHOW_GENERATION: AtomicU64 = AtomicU64::new(0);

/// How long the card's exit fade runs (`--duration-fast` in
/// app/consent/consent-window.css): the webview keeps the card mounted for
/// this long after its state clears, and the window has to stay mapped until
/// the fade has run or the card simply vanishes.
const HIDE_DELAY: Duration = Duration::from_millis(120);

/// Sizes the panel for `layout`, re-pins its top-right corner, and shows it.
///
/// The reposition is not redundant: a resize keeps the window's bottom-left
/// origin, so growing it without moving it would push the top edge up and
/// eventually off the menu bar. Setting the position again anchors the top
/// edge, and the panel grows downward.
pub fn show(app: &AppHandle, layout: ConsentPanelLayout) -> bool {
    let Some(window) = app.get_webview_window(CONSENT_PANEL_LABEL) else {
        return false;
    };
    SHOW_GENERATION.fetch_add(1, Ordering::SeqCst);
    let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize {
        width: PANEL_WIDTH,
        height: layout.height(),
    }));
    #[cfg(target_os = "macos")]
    if let Some((x, y)) = position(app) {
        let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
    }
    window.show().is_ok()
}

/// Unmaps the panel once the card's exit fade has run, unless a newer `show`
/// claimed the window in the meantime.
pub fn hide(app: &AppHandle) {
    let Some(window) = app.get_webview_window(CONSENT_PANEL_LABEL) else {
        return;
    };
    // Snapshot before anything observable happens, so any show that lands
    // after this point invalidates the delayed hide below.
    let scheduled_at = SHOW_GENERATION.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(HIDE_DELAY);
        if SHOW_GENERATION.load(Ordering::SeqCst) != scheduled_at {
            log::debug!("Skipping stale consent panel hide: a newer state is showing the panel");
            return;
        }
        let _ = window.hide();
    });
}

#[cfg(target_os = "macos")]
fn position(app: &AppHandle) -> Option<(f64, f64)> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let work_area = monitor.work_area();
    let left = f64::from(work_area.position.x) / scale;
    let top = f64::from(work_area.position.y) / scale;
    let width = f64::from(work_area.size.width) / scale;
    Some((
        left + width - PANEL_WIDTH - PANEL_MARGIN,
        top + PANEL_MARGIN,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ritual cards stay bounded rather than expanding toward the full app
    /// window when their source data grows.
    #[test]
    fn every_state_stays_within_the_ritual_panel_bound() {
        let states = [
            ConsentPanelLayout::Recording {
                disclosure_note: true,
            },
            ConsentPanelLayout::Prompt {
                always_record_checkbox: true,
                series_brief: true,
            },
            ConsentPanelLayout::Prep {
                loop_rows: 2,
                waiting_on: true,
                participants: true,
            },
            ConsentPanelLayout::Wrap { loop_delta: true },
        ];

        for state in states {
            assert!(
                state.height() < 300.0,
                "{state:?} exceeds the bounded ritual panel height"
            );
        }
    }
}
