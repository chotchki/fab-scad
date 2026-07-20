//! W.3.29.7: clipboard PASTE into the web editor. bevy_egui's own paste path is dead on desktop web —
//! it relies on a browser `paste` DOM event, which only fires when an editable element is focused. Its
//! hidden text-agent `<input>` (the would-be editable) is focused ONLY on mobile (the focus logic is
//! `is_mobile()`-gated), and the canvas isn't editable, so no `paste` event ever reaches egui. On top of
//! that, winit's `prevent_default_event_handling` suppresses the native paste. Net effect: typing works
//! (winit forwards keys straight to egui) but Cmd/Ctrl+V AND egui's right-click "Paste" both no-op,
//! because egui's clipboard buffer is never filled.
//!
//! The bridge: intercept Cmd/Ctrl+V ourselves, read the system clipboard via the async Clipboard API, and
//! hand egui the SAME `egui::Event::Paste` its own `web_clipboard` path would — so the text lands at the
//! cursor of the focused `TextEdit`. We never touch DOM focus, so typing is untouched (focusing the
//! text-agent input instead would break it — bevy_egui never wired that input's keydown→egui on desktop).
//!
//! COPY/CUT are unaffected: egui → `PlatformOutput.copied_text` → the async clipboard WRITE already works.
//! Browser support for `readText()`: Chromium + Safari honor it on a user gesture in a secure context
//! (https, and localhost, both qualify); Firefox restricts it to the real paste event — the known gap.
//!
//! Wasm only.
#![cfg(target_arch = "wasm32")]

use crate::*;
use bevy::input::{ButtonState, keyboard::KeyboardInput};
use bevy_egui::input::EguiInputEvent;

/// The async bridge from a clipboard read (off in a JS promise) back to a Bevy system. `async-channel`
/// because its ends are `Send + Sync` (Resource-safe); unbounded so the send never blocks.
#[derive(Resource)]
pub(crate) struct WebPaste {
    tx: async_channel::Sender<String>,
    rx: async_channel::Receiver<String>,
}

impl Default for WebPaste {
    fn default() -> Self {
        let (tx, rx) = async_channel::unbounded();
        Self { tx, rx }
    }
}

/// On Cmd/Ctrl+V, fire off an async clipboard read. winit forwards the keypress to Bevy (that's why we can
/// detect it here at all), but the browser's own `paste` event never reaches egui — so we read the
/// clipboard directly and let [`web_paste_apply`] inject it. Accepts either Super (macOS ⌘) or Control so
/// it works whatever the platform mapping. A failed read (permission denied, unfocused, Firefox) no-ops.
///
/// Detected off the keydown EVENT, not [`ButtonInput::just_pressed`]: on macOS the browser swallows a
/// letter key's `keyup` while Cmd is held, so V would stay "pressed" and only the FIRST Cmd+V would edge.
/// The modifier is read from [`ButtonInput`] — modifier keyups fire fine; it's only the letter's that's eaten.
pub(crate) fn web_paste_kick(
    mut keydowns: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    bridge: Res<WebPaste>,
) {
    let mut want_paste = false;
    for ev in keydowns.read() {
        // `!repeat` so a held V doesn't spam reads; `key_code` (physical) matches whatever 'V' maps to.
        if ev.state == ButtonState::Pressed && !ev.repeat && ev.key_code == KeyCode::KeyV {
            want_paste = true;
        }
    }
    let modifier = keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight);
    if !want_paste || !modifier {
        return;
    }
    let Some(clipboard) = web_sys::window().map(|w| w.navigator().clipboard()) else {
        return;
    };
    let tx = bridge.tx.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match wasm_bindgen_futures::JsFuture::from(clipboard.read_text()).await {
            Ok(val) => {
                if let Some(text) = val.as_string() {
                    // Unbounded, so this can only fail if the app is tearing down — nothing to do then.
                    let _ = tx.try_send(text);
                }
            }
            Err(_) => {
                bevy::log::warn!(
                    "clipboard read for paste failed (permission denied, unfocused, or unsupported browser)"
                );
            }
        }
    });
}

/// Drain clipboard text read by [`web_paste_kick`] and hand it to the PRIMARY egui context as a Paste
/// event — the very event bevy_egui's own `web_clipboard` system emits, so egui inserts it at the focused
/// `TextEdit`'s cursor. A one-frame lag behind the keypress; imperceptible. No focused text field ⇒ egui
/// simply ignores the event, so an errant Cmd+V elsewhere is harmless.
pub(crate) fn web_paste_apply(
    bridge: Res<WebPaste>,
    ctx: Query<Entity, With<PrimaryEguiContext>>,
    mut writer: MessageWriter<EguiInputEvent>,
) {
    let Ok(context) = ctx.single() else {
        return;
    };
    while let Ok(text) = bridge.rx.try_recv() {
        if text.is_empty() {
            continue;
        }
        writer.write(EguiInputEvent {
            context,
            event: egui::Event::Paste(text),
        });
    }
}
