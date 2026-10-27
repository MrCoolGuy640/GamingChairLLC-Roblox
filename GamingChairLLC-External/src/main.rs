#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! GamingChairLLC (Rust) entry point.
//!
//! Wires together the same pieces as the Python app:
//!
//!     config        paths + constants
//!     logging       console + rotating file log
//!     bundle        UI embedded in the executable by build.rs
//!     web_server    serves the bundled UI to the WebView
//!     backend       Engine + backend loop + event bus
//!     api           the JS-callable surface
//!     win32_style   dark title bar / window icon

mod api;
mod backend;
mod bundle;
mod cheats;
mod config;
mod events;
mod logging;
// The SDK deliberately mirrors the Python SDK's public surface; not every
// helper is used by the built-in cheats, but the object layer stays complete.
#[allow(dead_code)]
mod sdk;
mod web_server;
mod webview;
mod win32_style;

use std::sync::Arc;
use std::thread;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Theme, WindowBuilder};
use wry::{WebContext, WebViewBuilder, WebViewBuilderExtWindows};

use crate::events::AppEvent;

fn main() -> anyhow::Result<()> {
    let paths = config::Paths::discover();
    logging::init(&paths)?;

    if let Err(err) = paths.ensure_dirs() {
        log::warn!(target: "GamingChairLLC", "Could not create data folders: {err}");
    }

    // The window icon ships inside the bundle; Win32 wants a file path.
    let icon_path = paths.data_dir.join("icon.ico");
    if let Err(err) = bundle::write_icon(&icon_path) {
        log::warn!(target: "GamingChairLLC", "Could not write the window icon: {err}");
    }

    log::info!(
        target: "GamingChairLLC",
        "Starting {} {} ({})",
        config::APP_NAME,
        config::APP_VERSION,
        config::BUILD_TYPE
    );
    log::debug!(
        target: "GamingChairLLC",
        "{} bundled files | App data: {}",
        bundle::len(),
        paths.app_data.display()
    );

    let server = web_server::ResourceServer::start()?;
    let url = format!("{}/web/index.html", server.base_url());

    let mut event_loop_builder = EventLoopBuilder::<AppEvent>::with_user_event();
    let event_loop = event_loop_builder.build();
    let proxy = event_loop.create_proxy();
    let bus = events::EventBus::new(proxy.clone());
    let engine = backend::Engine::new(paths.clone(), bus.clone());

    let window = WindowBuilder::new()
        .with_title(config::WINDOW_TITLE)
        .with_inner_size(LogicalSize::new(
            config::WINDOW_WIDTH,
            config::WINDOW_HEIGHT,
        ))
        .with_min_inner_size(LogicalSize::new(
            config::WINDOW_WIDTH,
            config::WINDOW_HEIGHT,
        ))
        .with_resizable(true)
        .with_theme(Some(Theme::Dark))
        .build(&event_loop)?;

    let ipc_engine = Arc::clone(&engine);
    let ipc_proxy = proxy.clone();

    // Keep WebView2's browser profile under our app data folder instead of the
    // default `GamingChairLLC.exe.WebView2` folder next to the executable.
    let webview_data_dir = paths.app_data.join("webview");
    if let Err(err) = std::fs::create_dir_all(&webview_data_dir) {
        log::warn!(
            target: "GamingChairLLC",
            "Could not create the WebView data folder {}: {err}",
            webview_data_dir.display()
        );
    }
    let mut web_context = WebContext::new(Some(webview_data_dir));

    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_url(url)
        .with_initialization_script(webview::INIT_SCRIPT)
        .with_background_color(config::WINDOW_BG)
        // Devtools, browser shortcuts (Ctrl+R, F5, F12, Ctrl+Shift+I, ...) and
        // the right-click menu are development-build conveniences only.
        .with_devtools(config::DEVTOOLS)
        .with_browser_accelerator_keys(config::DEVTOOLS)
        .with_default_context_menus(config::DEVTOOLS)
        .with_ipc_handler(move |request| {
            // Every call gets its own worker thread, so a slow attach (offsets
            // download, DataModel wait) never blocks the UI thread.
            let body = request.body().clone();
            let engine = Arc::clone(&ipc_engine);
            let proxy = ipc_proxy.clone();

            let spawned = thread::Builder::new()
                .name("api".into())
                .spawn(move || {
                    let Some(request) = webview::parse_ipc(&body) else {
                        log::warn!(
                            target: "GamingChairLLC.api",
                            "Ignoring malformed IPC message"
                        );
                        return;
                    };

                    let result = api::dispatch(&engine, &request.method, &request.args);
                    let script = webview::resolve_js(request.id, &result);
                    let _ = proxy.send_event(AppEvent::Eval(script));
                });

            if let Err(err) = spawned {
                log::error!(
                    target: "GamingChairLLC.api",
                    "Could not spawn API worker: {err}"
                );
            }
        })
        .build(&window)?;

    win32_style::apply(&window, &icon_path);

    let mut webview = Some(webview);
    let mut window = Some(window);
    let shutdown_engine = Arc::clone(&engine);
    let shutdown_bus = bus.clone();
    let mut needs_shutdown = true;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(AppEvent::Eval(script)) => {
                if let Some(webview) = webview.as_ref() {
                    let _ = webview.evaluate_script(&script);
                }
            }
            Event::UserEvent(AppEvent::Emit { name, data }) => {
                if let Some(webview) = webview.as_ref() {
                    let script = webview::emit_js(&name, &data);
                    let _ = webview.evaluate_script(&script);
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if needs_shutdown {
                    needs_shutdown = false;
                    shutdown_bus.close();
                    shutdown_engine.shutdown();
                }
                *control_flow = ControlFlow::Exit;
            }
            Event::LoopDestroyed => {
                if needs_shutdown {
                    needs_shutdown = false;
                    shutdown_bus.close();
                    shutdown_engine.shutdown();
                }
                drop(webview.take());
                drop(window.take());
            }
            _ => {}
        }
    });
}
