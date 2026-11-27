//! Terminal clipboard: copy a selection out, paste into a session.
//!
//! ## Paste never passes through the webview
//!
//! The obvious design is a `clipboard_read` command returning text to the
//! webview, which then writes it to the PTY. That would make the clipboard a
//! readable secret: people copy passwords and tokens constantly, and CLAUDE.md
//! §18.14's rule is that no command hands a secret to the webview. So paste is
//! `session_paste(ids, selection)`: Rust reads the clipboard and writes it to
//! the sessions itself, and the text never crosses IPC. The webview can cause a
//! paste — it cannot read what was pasted.
//!
//! Copy goes the other way and is harmless: the text is already on screen.
//!
//! ## Linux: the window's own GTK clipboard
//!
//! The webview is a GTK widget, so the GTK clipboard is the one that is correct
//! on X11 **and** native Wayland, and it is the only route to the PRIMARY
//! selection (select-to-copy, middle-click-to-paste) that every Linux terminal
//! user expects. `gtk` is already in the build through Tauri — no new crate.
//! GTK is not thread-safe, so every call is marshalled to the main thread.
//!
//! Other platforms answer [`UNSUPPORTED`] and the frontend falls back to
//! `navigator.clipboard`, then hands the text to `session_paste_text`. PRIMARY
//! does not exist there and is silently skipped.

use serde::Deserialize;

use crate::session::{registry, SessionId};

/// Returned where there is no native implementation; the frontend checks for
/// this exact string and falls back to the web clipboard API.
pub const UNSUPPORTED: &str = "clipboard: unsupported";

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Selection {
    /// Ctrl+Shift+C / Ctrl+Shift+V.
    Clipboard,
    /// X11/Wayland PRIMARY: set by selecting, pasted by middle-click.
    Primary,
}

/// Paste `text` into each session. Every target gets its own encoding, since
/// one pane may be in bracketed-paste mode and its neighbour not.
pub fn paste_into(ids: &[SessionId], text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let mut first_err = None;
    for &id in ids {
        // One dead pane in a broadcast group must not stop the others.
        if let Err(e) = registry().get(id).and_then(|s| s.paste(text)) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

#[cfg(target_os = "linux")]
mod native {
    use super::Selection;
    use gtk::gdk;
    use tauri::AppHandle;
    use tokio::sync::oneshot;

    fn clipboard(sel: Selection) -> gtk::Clipboard {
        gtk::Clipboard::get(&match sel {
            Selection::Clipboard => gdk::SELECTION_CLIPBOARD,
            Selection::Primary => gdk::SELECTION_PRIMARY,
        })
    }

    pub async fn write(app: &AppHandle, sel: Selection, text: String) -> Result<(), String> {
        app.run_on_main_thread(move || clipboard(sel).set_text(&text))
            .map_err(|e| e.to_string())
    }

    /// `None` when the selection is empty or holds no text (an image, say).
    pub async fn read(app: &AppHandle, sel: Selection) -> Result<Option<String>, String> {
        let (tx, rx) = oneshot::channel();
        // `request_text` rather than `wait_for_text`: the latter spins a nested
        // main loop, re-entering every other GTK callback while it waits.
        app.run_on_main_thread(move || {
            clipboard(sel).request_text(move |_, text| {
                let _ = tx.send(text.map(str::to_owned));
            });
        })
        .map_err(|e| e.to_string())?;
        rx.await.map_err(|_| "clipboard request dropped".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
mod native {
    use super::{Selection, UNSUPPORTED};
    use tauri::AppHandle;

    pub async fn write(_: &AppHandle, _: Selection, _: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub async fn read(_: &AppHandle, _: Selection) -> Result<Option<String>, String> {
        Err(UNSUPPORTED.into())
    }
}

pub use native::{read, write};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_names_match_the_frontend() {
        let c: Selection = serde_json::from_str("\"clipboard\"").unwrap();
        let p: Selection = serde_json::from_str("\"primary\"").unwrap();
        assert_eq!((c, p), (Selection::Clipboard, Selection::Primary));
        assert!(serde_json::from_str::<Selection>("\"secondary\"").is_err());
    }

    #[test]
    fn pasting_nothing_into_nothing_is_not_an_error() {
        assert!(paste_into(&[], "x").is_ok());
        assert!(paste_into(&[u32::MAX], "").is_ok());
    }

    #[test]
    fn an_unknown_session_is_reported_not_ignored() {
        assert!(paste_into(&[u32::MAX], "x").is_err());
    }
}
