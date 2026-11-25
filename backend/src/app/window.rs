//! The main window: created hidden, placed where the user left it (if that
//! is still on a screen), shown once the page has loaded, and its placement
//! persisted as it moves/resizes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Url, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

use crate::app::events::{self, WINDOW_STATE_CHANGED};
use crate::app::updater;
use crate::config::IS_DEV;
use crate::store::WindowStateData;
use crate::Ctx;

pub const LABEL: &str = "main";
pub const DEFAULT_WIDTH: f64 = 1100.0;
pub const DEFAULT_HEIGHT: f64 = 980.0;
pub const MIN_WIDTH: f64 = 880.0;
// 720 covers 1366×768 minus the taskbar — the floor of mainstream laptops.
pub const MIN_HEIGHT: f64 = 720.0;
const MAX_SIZE: f64 = 10_000.0;
/// A saved rect counts as visible when at least this much of it overlaps a
/// monitor's work area (half off-screen windows are legitimate).
const MIN_VISIBLE: i64 = 100;
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(300);
/// Show the window even if the page never reports "loaded".
const SHOW_FALLBACK: Duration = Duration::from_secs(5);
const UPDATER_START_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub is_maximized: bool,
    pub is_fullscreen: bool,
}

/// Physical-pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Saved position and size, verified to be on a screen.
    Restore(Rect),
    /// Saved size without a position: centre it.
    Centered { width: u32, height: u32 },
    /// Nothing usable saved: default size, centred.
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InitialBounds {
    pub placement: Placement,
    pub is_maximized: bool,
    pub is_fullscreen: bool,
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

pub fn is_visible(rect: Rect, work_areas: &[Rect]) -> bool {
    work_areas.iter().any(|area| {
        let overlap = |start: i32, len: u32, area_start: i32, area_len: u32| {
            let end = i64::from(start) + i64::from(len);
            let area_end = i64::from(area_start) + i64::from(area_len);
            end.min(area_end) - i64::from(start).max(i64::from(area_start))
        };
        overlap(rect.x, rect.width, area.x, area.width) >= MIN_VISIBLE
            && overlap(rect.y, rect.height, area.y, area.height) >= MIN_VISIBLE
    })
}

/// Validates the saved placement against the current monitors. A window
/// saved on a monitor that is gone falls back to a centred default — never
/// open it where the user can't see it.
pub fn initial_bounds(saved: Option<&WindowStateData>, work_areas: &[Rect]) -> InitialBounds {
    let Some(saved) = saved else {
        return InitialBounds {
            placement: Placement::Default,
            is_maximized: false,
            is_fullscreen: false,
        };
    };
    let width = clamp(saved.width, MIN_WIDTH, MAX_SIZE) as u32;
    let height = clamp(saved.height, MIN_HEIGHT, MAX_SIZE) as u32;
    let placement = match (saved.x, saved.y) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
            let rect = Rect {
                x: x.clamp(i32::MIN as f64, i32::MAX as f64) as i32,
                y: y.clamp(i32::MIN as f64, i32::MAX as f64) as i32,
                width,
                height,
            };
            if is_visible(rect, work_areas) {
                Placement::Restore(rect)
            } else {
                Placement::Default
            }
        }
        _ => Placement::Centered { width, height },
    };
    InitialBounds {
        placement,
        is_maximized: saved.is_maximized,
        is_fullscreen: saved.is_fullscreen,
    }
}

/// `width`×`height` centred in `area`, shrunk to fit it (so the custom title
/// bar is never above the top of the screen).
pub fn centered(area: Rect, width: u32, height: u32) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + ((area.width - width) / 2) as i32,
        y: area.y + ((area.height - height) / 2) as i32,
        width,
        height,
    }
}

/// Remembers the *normal* (not maximised/fullscreen/minimised) bounds, so
/// restoring after a maximise gives back a sensible window.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tracker {
    normal: Option<Rect>,
    maximized: bool,
    fullscreen: bool,
}

impl Tracker {
    /// Records the current window geometry. Returns true when the
    /// maximised/fullscreen flags changed. Minimised states are ignored:
    /// Windows reports a bogus position and "not maximised" while minimised.
    pub fn observe(
        &mut self,
        rect: Rect,
        maximized: bool,
        fullscreen: bool,
        minimized: bool,
    ) -> bool {
        if minimized {
            return false;
        }
        if !maximized && !fullscreen {
            self.normal = Some(rect);
        }
        let changed = (maximized, fullscreen) != (self.maximized, self.fullscreen);
        self.maximized = maximized;
        self.fullscreen = fullscreen;
        changed
    }

    pub fn state(&self) -> WindowState {
        WindowState {
            is_maximized: self.maximized,
            is_fullscreen: self.fullscreen,
        }
    }

    pub fn snapshot(&self) -> Option<WindowStateData> {
        let normal = self.normal?;
        Some(WindowStateData {
            width: f64::from(normal.width),
            height: f64::from(normal.height),
            x: Some(f64::from(normal.x)),
            y: Some(f64::from(normal.y)),
            is_maximized: self.maximized,
            is_fullscreen: self.fullscreen,
        })
    }
}

#[derive(Default)]
pub struct WindowPersist {
    tracker: Mutex<Tracker>,
    generation: AtomicU64,
    shown: AtomicBool,
}

impl WindowPersist {
    fn tracker(&self) -> MutexGuard<'_, Tracker> {
        self.tracker.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn rect_of(area: &tauri::PhysicalRect<i32, u32>) -> Rect {
    Rect {
        x: area.position.x,
        y: area.position.y,
        width: area.size.width,
        height: area.size.height,
    }
}

/// Reads the live window state into the tracker; emits
/// `window:state:changed` and saves right away when the flags changed.
fn sync(app: &AppHandle, window: &WebviewWindow) {
    let (Ok(position), Ok(size)) = (window.outer_position(), window.inner_size()) else {
        return;
    };
    let rect = Rect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    let maximized = window.is_maximized().unwrap_or(false);
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    let minimized = window.is_minimized().unwrap_or(false);

    let ctx = app.state::<Ctx>();
    let (changed, state) = {
        let mut tracker = ctx.window.tracker();
        let changed = tracker.observe(rect, maximized, fullscreen, minimized);
        (changed, tracker.state())
    };
    if changed {
        events::emit(app, WINDOW_STATE_CHANGED, state);
        save(app);
    }
}

fn save(app: &AppHandle) {
    let ctx = app.state::<Ctx>();
    let snapshot = ctx.window.tracker().snapshot();
    if let Some(snapshot) = snapshot {
        ctx.store.update(|d| d.window_state = Some(snapshot));
    }
}

/// Debounced save: dragging fires many move/resize events.
fn schedule_save(app: &AppHandle, window: &WebviewWindow) {
    let ctx = app.state::<Ctx>();
    let generation = ctx.window.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let (app, window) = (app.clone(), window.clone());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(PERSIST_DEBOUNCE).await;
        if app.state::<Ctx>().window.generation.load(Ordering::SeqCst) == generation {
            sync(&app, &window);
            save(&app);
        }
    });
}

/// Saves the current placement immediately (on close, before an update
/// installer exits the process).
pub fn save_now(app: &AppHandle) {
    app.state::<Ctx>()
        .window
        .generation
        .fetch_add(1, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window(LABEL) {
        sync(app, &window);
    }
    save(app);
}

pub fn state(window: &WebviewWindow) -> WindowState {
    WindowState {
        is_maximized: window.is_maximized().unwrap_or(false),
        is_fullscreen: window.is_fullscreen().unwrap_or(false),
    }
}

/// Brings the main window to the front: what a second launch of the
/// launcher does instead of starting another one.
pub fn focus(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn toggle_maximize(app: &AppHandle, window: &WebviewWindow) {
    // Fullscreen wins: double-clicking the title bar in fullscreen leaves
    // fullscreen instead of toggling maximise underneath it.
    let result = if window.is_fullscreen().unwrap_or(false) {
        window.set_fullscreen(false)
    } else if window.is_maximized().unwrap_or(false) {
        window.unmaximize()
    } else {
        window.maximize()
    };
    if let Err(error) = result {
        log::warn!("toggle maximize failed: {error}");
    }
    sync(app, window);
}

pub fn toggle_fullscreen(app: &AppHandle, window: &WebviewWindow) {
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    if let Err(error) = window.set_fullscreen(!fullscreen) {
        log::warn!("toggle fullscreen failed: {error}");
    }
    sync(app, window);
}

/// Where the frontend is served from: the Vite dev server in `tauri dev`,
/// Tauri's asset protocol otherwise.
fn app_url(app: &AppHandle) -> Url {
    #[cfg(dev)]
    if let Some(url) = app.config().build.dev_url.clone() {
        return url;
    }
    let _ = app;
    Url::parse("http://tauri.localhost/").expect("static URL")
}

/// Navigations allowed inside the window: our own origin only.
pub fn is_app_url(url: &Url, app_url: &Url) -> bool {
    url.as_str() == "about:blank" || url.origin() == app_url.origin()
}

/// Turns off the default context menu (and, in release, browser
/// accelerators: reload, print, find, zoom, devtools...). WebView2 applies
/// setting changes from the next top-level navigation, which is why the
/// window starts on about:blank and only then navigates to the app.
fn apply_webview_settings(window: &WebviewWindow) {
    let result = window.with_webview(|webview| {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
        use windows::core::Interface;

        let apply = || -> windows::core::Result<()> {
            unsafe {
                let settings = webview.controller().CoreWebView2()?.Settings()?;
                settings.SetAreDefaultContextMenusEnabled(false)?;
                if !IS_DEV {
                    settings
                        .cast::<ICoreWebView2Settings3>()?
                        .SetAreBrowserAcceleratorKeysEnabled(false)?;
                }
            }
            Ok(())
        };
        if let Err(error) = apply() {
            log::warn!("could not apply WebView2 settings: {error}");
        }
    });
    if let Err(error) = result {
        log::warn!("with_webview failed: {error}");
    }
}

fn show(app: &AppHandle, window: &WebviewWindow) {
    if app.state::<Ctx>().window.shown.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(error) = window.show() {
        log::error!("could not show the window: {error}");
    }
    let _ = window.set_focus();
    // The frontend subscribes to `updater:*` on mount; start a little after
    // the page is up so no event is emitted before anyone listens.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(UPDATER_START_DELAY).await;
        updater::start(&app);
    });
}

/// Applies the saved (or default) placement to the still-hidden window and
/// returns the tracker seeded with it.
fn place(window: &WebviewWindow, saved: Option<&WindowStateData>) -> Tracker {
    let monitors = window.available_monitors().unwrap_or_default();
    let areas: Vec<Rect> = monitors.iter().map(|m| rect_of(m.work_area())).collect();
    let primary = window
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.first().cloned());
    let (primary_area, scale) = primary
        .map(|m| (rect_of(m.work_area()), m.scale_factor()))
        .unwrap_or((
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040,
            },
            1.0,
        ));

    let bounds = initial_bounds(saved, &areas);
    let rect = match bounds.placement {
        Placement::Restore(rect) => rect,
        Placement::Centered { width, height } => centered(primary_area, width, height),
        Placement::Default => centered(
            primary_area,
            (DEFAULT_WIDTH * scale).round() as u32,
            (DEFAULT_HEIGHT * scale).round() as u32,
        ),
    };
    // Position first: moving to a monitor with another DPI may rescale the
    // window, and the size we set afterwards is the one we want.
    let _ = window.set_position(PhysicalPosition::new(rect.x, rect.y));
    let _ = window.set_size(PhysicalSize::new(rect.width, rect.height));
    if bounds.is_maximized {
        let _ = window.maximize();
    }
    if bounds.is_fullscreen {
        let _ = window.set_fullscreen(true);
    }
    Tracker {
        normal: Some(rect),
        maximized: bounds.is_maximized,
        fullscreen: bounds.is_fullscreen,
    }
}

pub fn create(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == LABEL)
        .cloned()
        .ok_or("main window missing from tauri.conf.json")?;
    config.url = WebviewUrl::External(Url::parse("about:blank")?);

    let target = app_url(app);
    let nav_target = target.clone();
    let load_target = target.clone();
    let load_app = app.clone();

    // The webview never leaves the app: external links go through the
    // `open_external` command, so any other navigation or popup is dropped.
    let window = WebviewWindowBuilder::from_config(app, &config)?
        .on_navigation(move |url| {
            let allowed = is_app_url(url, &nav_target);
            if !allowed {
                log::warn!("blocked navigation to {url}");
            }
            allowed
        })
        .on_new_window(|url, _features| {
            log::warn!("blocked new window for {url}");
            NewWindowResponse::Deny
        })
        .on_page_load(move |window, payload| {
            if payload.event() == PageLoadEvent::Finished
                && payload.url().origin() == load_target.origin()
            {
                show(&load_app, &window);
            }
        })
        .build()?;

    apply_webview_settings(&window);

    let ctx = app.state::<Ctx>();
    let tracker = place(&window, ctx.store.get().window_state.as_ref());
    *ctx.window.tracker() = tracker;

    window.navigate(target)?;

    let events_app = app.clone();
    let events_window = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Resized(_) => {
            sync(&events_app, &events_window);
            schedule_save(&events_app, &events_window);
        }
        WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. } => {
            schedule_save(&events_app, &events_window);
        }
        WindowEvent::CloseRequested { .. } => save_now(&events_app),
        _ => {}
    });

    let fallback_app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SHOW_FALLBACK).await;
        if let Some(window) = fallback_app.get_webview_window(LABEL) {
            if !fallback_app
                .state::<Ctx>()
                .window
                .shown
                .load(Ordering::SeqCst)
            {
                log::warn!("page did not report loaded in time; showing the window anyway");
                show(&fallback_app, &window);
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PRIMARY: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1040,
    };
    const LEFT: Rect = Rect {
        x: -1920,
        y: 0,
        width: 1920,
        height: 1040,
    };

    fn saved(x: Option<f64>, y: Option<f64>) -> WindowStateData {
        WindowStateData {
            width: 1200.0,
            height: 900.0,
            x,
            y,
            is_maximized: true,
            is_fullscreen: false,
        }
    }

    #[test]
    fn visibility_needs_a_real_overlap() {
        let rect = |x, y| Rect {
            x,
            y,
            width: 1100,
            height: 980,
        };
        assert!(is_visible(rect(100, 50), &[PRIMARY]));
        assert!(is_visible(rect(-1000, 50), &[PRIMARY])); // half off-screen is fine
        assert!(is_visible(rect(1820, 940), &[PRIMARY])); // exactly 100×100 visible
        assert!(!is_visible(rect(1821, 50), &[PRIMARY]));
        assert!(!is_visible(rect(-1500, 50), &[PRIMARY]));
        assert!(is_visible(rect(-1500, 50), &[PRIMARY, LEFT]));
        assert!(!is_visible(rect(100, 50), &[]));
    }

    #[test]
    fn restores_only_visible_placements() {
        assert_eq!(
            initial_bounds(None, &[PRIMARY]),
            InitialBounds {
                placement: Placement::Default,
                is_maximized: false,
                is_fullscreen: false
            }
        );
        let on_screen = initial_bounds(Some(&saved(Some(10.0), Some(20.0))), &[PRIMARY]);
        assert_eq!(
            on_screen.placement,
            Placement::Restore(Rect {
                x: 10,
                y: 20,
                width: 1200,
                height: 900
            })
        );
        assert!(on_screen.is_maximized);

        // Monitor unplugged: default placement, flags kept.
        let gone = initial_bounds(Some(&saved(Some(-1500.0), Some(20.0))), &[PRIMARY]);
        assert_eq!(gone.placement, Placement::Default);
        assert!(gone.is_maximized);

        assert_eq!(
            initial_bounds(Some(&saved(None, None)), &[PRIMARY]).placement,
            Placement::Centered {
                width: 1200,
                height: 900
            }
        );
    }

    #[test]
    fn clamps_garbage_sizes() {
        let mut data = saved(Some(0.0), Some(0.0));
        data.width = f64::NAN;
        data.height = 50_000.0;
        let Placement::Restore(rect) = initial_bounds(Some(&data), &[PRIMARY]).placement else {
            panic!("expected restore");
        };
        assert_eq!((rect.width, rect.height), (880, 10_000));
    }

    #[test]
    fn centres_and_fits_the_work_area() {
        assert_eq!(
            centered(PRIMARY, 1100, 980),
            Rect {
                x: 410,
                y: 30,
                width: 1100,
                height: 980
            }
        );
        // 1366×768 laptop: the 980 px default is shrunk so the title bar stays visible.
        let laptop = Rect {
            x: 0,
            y: 0,
            width: 1366,
            height: 728,
        };
        assert_eq!(
            centered(laptop, 1100, 980),
            Rect {
                x: 133,
                y: 0,
                width: 1100,
                height: 728
            }
        );
        assert_eq!(centered(LEFT, 100, 100).x, -1010);
    }

    #[test]
    fn tracker_keeps_normal_bounds_across_maximise_and_minimise() {
        let normal = Rect {
            x: 100,
            y: 100,
            width: 1100,
            height: 980,
        };
        let mut tracker = Tracker::default();
        assert!(!tracker.observe(normal, false, false, false));
        assert!(tracker.observe(
            Rect {
                x: -8,
                y: -8,
                width: 1936,
                height: 1056
            },
            true,
            false,
            false
        ));
        // Minimised: Windows reports -32000,-32000 and "not maximised" — ignored.
        assert!(!tracker.observe(
            Rect {
                x: -32000,
                y: -32000,
                width: 160,
                height: 28
            },
            false,
            false,
            true
        ));
        assert!(tracker.observe(PRIMARY, true, true, false));

        let snapshot = tracker.snapshot().unwrap();
        assert_eq!((snapshot.x, snapshot.y), (Some(100.0), Some(100.0)));
        assert_eq!((snapshot.width, snapshot.height), (1100.0, 980.0));
        assert!(snapshot.is_maximized && snapshot.is_fullscreen);
        assert_eq!(
            serde_json::to_value(tracker.state()).unwrap(),
            json!({ "isMaximized": true, "isFullscreen": true })
        );
        assert_eq!(Tracker::default().snapshot(), None);
    }

    #[test]
    fn navigation_stays_on_the_app_origin() {
        let release = Url::parse("http://tauri.localhost/").unwrap();
        let dev = Url::parse("http://localhost:5173").unwrap();
        let ok = |u: &str, base: &Url| is_app_url(&Url::parse(u).unwrap(), base);
        assert!(ok("http://tauri.localhost/index.html#/home", &release));
        assert!(ok("about:blank", &release));
        assert!(ok("http://localhost:5173/src/main.ts", &dev));
        assert!(!ok("http://localhost:5174/", &dev));
        assert!(!ok("https://tauri.localhost/", &release));
        assert!(!ok("https://sarp.es/", &release));
        assert!(!ok("file:///C:/Windows/win.ini", &release));
    }
}
