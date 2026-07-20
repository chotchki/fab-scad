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

use std::sync::atomic::{AtomicBool, Ordering};

use crate::*;
use bevy::input::{ButtonState, keyboard::KeyboardInput};
use bevy_egui::input::EguiInputEvent;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

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

/// The browser's authoritative modifier state, captured from every key event's `metaKey`/`ctrlKey`/etc.
/// These reflect the REAL OS modifier state even when a `keyup` event is dropped (the macOS ⌘-keyup
/// swallow) — which is exactly why reading them fixes a stuck modifier that event-tracking can't.
static DOM_SHIFT: AtomicBool = AtomicBool::new(false);
static DOM_CTRL: AtomicBool = AtomicBool::new(false);
static DOM_ALT: AtomicBool = AtomicBool::new(false);
static DOM_META: AtomicBool = AtomicBool::new(false);

/// Install document `keydown`/`keyup` listeners (Startup, wasm) that record the browser's live modifier
/// state into the atomics above. bevy_egui derives modifier state from key PRESS/RELEASE events, so a
/// dropped keyup leaves it stuck; the event's `metaKey`/`ctrlKey`/`altKey`/`shiftKey` booleans carry the
/// true current state regardless, so [`sync_modifiers`] can override the stuck flag. Closures are leaked.
pub(crate) fn install_modifier_watch() {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        bevy::log::warn!("no document: modifier watch not installed; web typing may stick");
        return;
    };
    for ev in ["keydown", "keyup"] {
        let closure =
            Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|e: web_sys::KeyboardEvent| {
                DOM_SHIFT.store(e.shift_key(), Ordering::Relaxed);
                DOM_CTRL.store(e.ctrl_key(), Ordering::Relaxed);
                DOM_ALT.store(e.alt_key(), Ordering::Relaxed);
                DOM_META.store(e.meta_key(), Ordering::Relaxed);
            });
        let _ = doc.add_event_listener_with_callback(ev, closure.as_ref().unchecked_ref());
        closure.forget();
    }
}

/// Keep bevy_egui's [`ModifierKeysState`] honest (W.3.29.8): overwrite it with the browser's ground-truth
/// modifier state each frame. bevy_egui blocks ALL text input while Cmd/Ctrl reads as "held"
/// ([`text_input_is_allowed`], so Cmd+A doesn't type "a"), and it only clears that on a `KeyboardFocusLost`
/// event — which winit NEVER fires on web. A ⌘ keyup dropped by macOS/the browser then leaves `win` stuck
/// true and kills ALL typing (the "click the URL bar and back" workaround just forces the focus-lost
/// reset). Reconciling against Bevy's `ButtonInput` doesn't help — it's stuck on the same missed keyup —
/// but the DOM `metaKey`/etc booleans reflect the real OS state, so this can't get stuck.
///
/// Ordered before bevy_egui's keyboard reader so the corrected state applies to the SAME frame's keystroke
/// (no lost first character). A genuine modifier keydown re-sets the atomic before this runs, so real
/// shortcuts are unaffected.
///
/// [`text_input_is_allowed`]: bevy_egui::input::ModifierKeysState::text_input_is_allowed
/// [`ModifierKeysState`]: bevy_egui::input::ModifierKeysState
pub(crate) fn sync_modifiers(mut mods: ResMut<bevy_egui::input::ModifierKeysState>) {
    let (s, c, a, m) = (
        DOM_SHIFT.load(Ordering::Relaxed),
        DOM_CTRL.load(Ordering::Relaxed),
        DOM_ALT.load(Ordering::Relaxed),
        DOM_META.load(Ordering::Relaxed),
    );
    // Only touch the resource when something differs — avoid churning change-detection every frame.
    if mods.shift != s || mods.ctrl != c || mods.alt != a || mods.win != m {
        mods.shift = s;
        mods.ctrl = c;
        mods.alt = a;
        mods.win = m;
    }
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
