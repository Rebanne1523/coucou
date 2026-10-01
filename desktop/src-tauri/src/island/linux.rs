// Linux side of the island.
//
// Wayland (Plasma, Sway, Hyprland…): a normal toplevel cannot place itself, stay
// above everything, or refuse focus — the compositor decides. The one protocol
// made for "a bar at the edge of the screen" is wlr-layer-shell, which KDE's
// KWin, Sway, Hyprland and others implement (GNOME's Mutter does not). The
// island becomes an `overlay` layer surface anchored to the top edge; with no
// left/right anchor the compositor centres it horizontally.
//
// X11, or a Wayland session without layer-shell: a plain undecorated, always-on-
// top window, positioned by hand like on Windows.
//
// Two things the Windows build gets from Win32 do not exist here, and both are
// handled differently:
//   * no global cursor position (Wayland forbids it) — so there is no cursor
//     poll. The webview receives real pointer events inside its own surface and
//     the front end feeds them to the island (see main.ts);
//   * no WS_EX_TRANSPARENT toggling — instead the surface's *input region* is set
//     to the island shape, so everything around it is click-through at the
//     compositor level and costs nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk::cairo::{RectangleInt, Region};
use gtk::gdk::{EventMask, NotifyType};
use gtk::glib::Propagation;
use gtk::prelude::*;
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tauri::{AppHandle, Emitter, PhysicalPosition, PhysicalSize, WebviewWindow};

use super::{current_screen_key, window, PollGate, HIT_MARGIN, PANEL_H, PANEL_W, WINDOW_LABEL};

/// True once the island window is a layer-shell surface.
static LAYER: AtomicBool = AtomicBool::new(false);

/// No global pointer on Wayland, and on X11 the front end does not need it:
/// the window sees the pointer whenever it is over the island.
pub fn cursor_physical() -> Option<(f64, f64)> {
    None
}

/// Turns the island window into a layer surface, or tunes it for X11.
///
/// Must run before the window is first shown: layer-shell is negotiated when the
/// surface is created, which is why tauri.linux.conf.json starts it hidden.
pub fn prepare(app: &AppHandle, win: &WebviewWindow) {
    let Ok(gtk_win) = win.gtk_window() else {
        crate::log::line("island: no GTK window — placement left to the compositor".to_string());
        return;
    };

    // The page never hears that the pointer left the window: WebKitGTK does not
    // fire `mouseleave` on the document for it. GTK does report it, so relay it —
    // the island needs it to know when to start its auto-close countdown.
    gtk_win.add_events(EventMask::LEAVE_NOTIFY_MASK);
    let handle = app.clone();
    gtk_win.connect_leave_notify_event(move |_, event| {
        // Crossing into a child widget (the webview) is not leaving the window.
        if event.detail() != NotifyType::Inferior {
            let _ = handle.emit_to(WINDOW_LABEL, "pointer-left", ());
        }
        Propagation::Proceed
    });

    if gtk_layer_shell::is_supported() {
        if gtk_win.is_realized() {
            // init_for_window only works on an unrealized window.
            gtk_win.hide();
            gtk_win.unrealize();
        }
        gtk_win.init_layer_shell();
        gtk_win.set_namespace("coucou");
        gtk_win.set_layer(Layer::Overlay);
        gtk_win.set_anchor(Edge::Top, true);
        // -1: sit at the very top edge, over any panel, instead of below it.
        gtk_win.set_exclusive_zone(-1);
        gtk_win.set_keyboard_mode(KeyboardMode::None);
        LAYER.store(true, Ordering::Relaxed);
        crate::log::line("island: wlr-layer-shell surface".to_string());
    } else {
        gtk_win.set_accept_focus(false);
        gtk_win.set_focus_on_map(false);
        gtk_win.set_type_hint(gtk::gdk::WindowTypeHint::Dock);
        gtk_win.set_skip_taskbar_hint(true);
        gtk_win.set_skip_pager_hint(true);
        crate::log::line("island: plain window (no layer-shell) — X11 mode".to_string());
    }
}

/// Lets the chat field take the keyboard, and takes it back afterwards.
pub fn set_activating(win: &WebviewWindow, activating: bool) {
    let Ok(gtk_win) = win.gtk_window() else { return };
    if LAYER.load(Ordering::Relaxed) {
        // OnDemand: the compositor gives us the keyboard when the user clicks in.
        gtk_win.set_keyboard_mode(if activating { KeyboardMode::OnDemand } else { KeyboardMode::None });
    } else {
        gtk_win.set_accept_focus(activating);
    }
}

/// Sizes the window; positions it only when nothing else does.
pub fn place(win: &WebviewWindow, x: i32, y: i32, pw: u32, ph: u32) {
    if LAYER.load(Ordering::Relaxed) {
        // The compositor positions a layer surface (top edge, centred). We only
        // say how big it wants to be, in logical pixels.
        if let Ok(gtk_win) = win.gtk_window() {
            let scale = gtk_win.scale_factor().max(1) as u32;
            let (w, h) = ((pw / scale).max(1) as i32, (ph / scale).max(1) as i32);
            gtk_win.set_size_request(w, h);
            gtk_win.resize(w, h);
        }
        return;
    }
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_position(PhysicalPosition::new(x, y));
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_always_on_top(true);
}

/// Click-through is the input region's job here; the per-window "ignore cursor"
/// flag would overwrite it, so it is deliberately not used.
pub fn set_ignore_cursor(_app: &AppHandle, _ignore: bool) {}

/// The island moved or resized: pass clicks through everywhere else.
pub fn island_rect_changed(app: &AppHandle, gate: &PollGate) {
    apply_input_region(app, gate);
}

pub fn collapsed_changed(app: &AppHandle, gate: &PollGate) {
    apply_input_region(app, gate);
}

fn apply_input_region(app: &AppHandle, gate: &PollGate) {
    let collapsed = gate.collapsed.load(Ordering::Relaxed);
    let rect = *gate.rect.lock().unwrap();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(win) = window(&handle) else { return };
        let Ok(gtk_win) = win.gtk_window() else { return };

        // The wake strip is tiny and has to take the pointer everywhere.
        if collapsed {
            gtk_win.input_shape_combine_region(None);
            return;
        }
        // Nothing pushed yet: pass everything through until the front end says
        // where the island is.
        if rect.w <= 0.0 {
            gtk_win.input_shape_combine_region(Some(&Region::create()));
            return;
        }
        let x0 = (rect.x - HIT_MARGIN).max(0.0).floor();
        let y0 = (rect.y - HIT_MARGIN).max(0.0).floor();
        let x1 = (rect.x + rect.w + HIT_MARGIN).min(PANEL_W).ceil();
        let y1 = (rect.y + rect.h + HIT_MARGIN).min(PANEL_H).ceil();
        let region = Region::create_rectangle(&RectangleInt::new(
            x0 as i32,
            y0 as i32,
            (x1 - x0).max(1.0) as i32,
            (y1 - y0).max(1.0) as i32,
        ));
        gtk_win.input_shape_combine_region(Some(&region));
    });
}

/// Watches for monitors being plugged, unplugged or rescaled. There is no cursor
/// to follow, so unlike Windows this wakes twice a second and only while the
/// island is on screen; hidden, it parks on the condvar and costs nothing.
pub fn spawn_watcher(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        loop {
            gate.wait_until_active();
            while gate.is_active() {
                std::thread::sleep(Duration::from_millis(500));
                let now = current_screen_key(&app);
                if now.is_some() && now != last_screen {
                    let first = last_screen.is_none();
                    last_screen = now;
                    if !first {
                        crate::log::line("display layout changed — repositioning".to_string());
                        let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                    }
                }
            }
        }
    });
}
