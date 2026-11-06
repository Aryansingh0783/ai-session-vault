//! Native window + tray for AI Session Vault.
//!
//! `avault app` starts the engine and the local UI server, then runs this program with the UI's
//! link. It shows the page in the system webview (WebView2 on Windows) and adds a tray icon.
//! Closing the window hides it to the tray (syncing continues); "Quit" exits, which stops the engine.

use anyhow::{anyhow, bail, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

/// 32x32 mark: dark rounded square with a white ring, matching the app logo.
fn icon() -> Icon {
    const N: u32 = 32;
    let mut px = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt();
            let (ax, ay) = (dx.abs() - 9.0, dy.abs() - 9.0);
            let inside = ax.max(0.0).hypot(ay.max(0.0)) + ax.max(ay).min(0.0) <= 6.5;
            let ring = (d - 7.0).abs() <= 1.6 || d <= 2.4;
            let (v, a) = if !inside { (0, 0) } else if ring { (255, 255) } else { (22, 255) };
            px.extend_from_slice(&[v, v, v, a]);
        }
    }
    Icon::from_rgba(px, N, N).expect("valid icon")
}

/// Fire-and-forget POST to the local UI server (the same API the page uses).
fn api(base: &str, token: &str, path: &str) {
    let (base, token, path) = (base.to_string(), token.to_string(), path.to_string());
    std::thread::spawn(move || {
        let _ = ureq::post(&format!("{base}{path}")).set("X-Token", &token).send_string("{}");
    });
}

fn main() -> Result<()> {
    let url = std::env::args().nth(1).ok_or_else(|| anyhow!("usage: avault-window <link printed by `avault ui`> (normally started by `avault app`)"))?;
    // http://127.0.0.1:PORT/?t=TOKEN
    let (base, token) = match url.split_once("/?t=") {
        Some((b, t)) => (b.to_string(), t.to_string()),
        None => bail!("not a vault link: {url}"),
    };

    let event_loop = EventLoopBuilder::<()>::new().build();
    let window = WindowBuilder::new()
        .with_title("Vault")
        .with_inner_size(LogicalSize::new(1360.0, 860.0))
        .with_min_inner_size(LogicalSize::new(900.0, 640.0))
        .build(&event_loop)
        .map_err(|e| anyhow!("cannot create the window: {e}"))?;

    let data = std::env::temp_dir().join("ai-vault-webview");
    let mut ctx = wry::WebContext::new(Some(data));
    let _view = wry::WebViewBuilder::with_web_context(&mut ctx)
        .with_url(&url)
        .build(&window)
        .map_err(|e| anyhow!("cannot start the webview (on Windows this needs the WebView2 runtime): {e}"))?;

    let menu = Menu::new();
    let open = MenuItem::new("Open Vault", true, None);
    let sync = MenuItem::new("Sync now", true, None);
    let pause = MenuItem::new("Pause syncing", true, None);
    let resume = MenuItem::new("Resume syncing", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &sync, &pause, &resume, &PredefinedMenuItem::separator(), &quit]).map_err(|e| anyhow!("{e}"))?;
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Vault")
        .with_icon(icon())
        .build()
        .map_err(|e| anyhow!("cannot create the tray icon: {e}"))?;

    let (open_id, sync_id, pause_id, resume_id, quit_id) = (open.id().clone(), sync.id().clone(), pause.id().clone(), resume.id().clone(), quit.id().clone());
    let (menu_rx, tray_rx) = (MenuEvent::receiver(), TrayIconEvent::receiver());

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(150));
        let show = || {
            window.set_visible(true);
            window.set_minimized(false);
            window.set_focus();
        };
        while let Ok(e) = menu_rx.try_recv() {
            if e.id == open_id {
                show();
            } else if e.id == sync_id {
                api(&base, &token, "/api/sync");
            } else if e.id == pause_id {
                api(&base, &token, "/api/pause");
            } else if e.id == resume_id {
                api(&base, &token, "/api/resume");
            } else if e.id == quit_id {
                *flow = ControlFlow::Exit;
            }
        }
        while let Ok(e) = tray_rx.try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show();
            }
        }
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            window.set_visible(false); // keep syncing from the tray
        }
    });
}
