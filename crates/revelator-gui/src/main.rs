#![windows_subsystem = "windows"] // GUI app: do not open a console window

mod app;
mod autostart;
mod demo;
mod dsp;
mod icon;
mod map;
mod model;
mod presets;
mod profiles;
mod real;
mod shell;
mod taskbar;
mod tips;
mod tray;
mod update;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    // Set by the "Start on boot" entry: start hidden in the tray.
    let minimized = args.iter().any(|a| a == autostart::MINIMIZED_FLAG);
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    // Jump list tasks launch the app with an action ("--slot 2", "--toggle-mute").
    let action = model::Action::from_args(&args);
    // Only one real instance: a second launch hands its action to the existing one, or just
    // brings its window back.
    if !tray::acquire_single_instance(action) {
        return Ok(());
    }
    // A new build/location of the app takes over an existing "Start on boot" entry.
    // Not in demo mode, so a throwaway demo run does not retarget it.
    if !demo {
        let _ = autostart::refresh();
    }
    let tab = flag("--tab");
    let size = flag("--size")
        .and_then(|s| {
            let (w, h) = s.split_once(['x', 'X'])?;
            Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])
        })
        .unwrap_or([1100.0, 760.0]);
    let backend: Box<dyn model::Backend> = if demo {
        Box::new(demo::DemoBackend::new())
    } else {
        Box::new(real::RealBackend::spawn(
            Box::new(real::HwOpener),
            real::Timing::default(),
        ))
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([640.0, 480.0])
            .with_title(taskbar::TITLE)
            .with_icon(egui::IconData {
                rgba: icon::rgba(64),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };
    eframe::run_native(
        taskbar::TITLE,
        options,
        Box::new(move |cc| {
            app::setup_style(&cc.egui_ctx);
            let mut a = app::App::new(backend, demo);
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
                _ => 0,
            };
            if hwnd != 0 {
                a.hwnd = hwnd;
                tray::mark_main_window(hwnd);
                a.tray = tray::create(&cc.egui_ctx, hwnd);
                a.attach_taskbar(hwnd, &cc.egui_ctx);
                // Only hide when the tray icon exists, otherwise there is no way back.
                a.start_hidden = minimized && a.tray.is_some();
            }
            if !demo {
                a.start_update_checks(&cc.egui_ctx);
            }
            a.startup_action = action;
            if let Some(t) = &tab {
                a.select_tab(t);
            }
            Ok(Box::new(a))
        }),
    )
}
