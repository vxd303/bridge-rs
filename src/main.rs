#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env,
    fs::{create_dir_all, OpenOptions},
    future::IntoFuture,
    io::Write,
    panic,
    path::PathBuf,
    sync::OnceLock,
    thread,
    time::{Duration, Instant, SystemTime},
};

use auto_launch::AutoLaunchBuilder;
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket},
        Request, WebSocketUpgrade,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use http::{HeaderValue, Method, StatusCode};
use reqwest::Url;
use rfd::MessageDialog;
use tao::event_loop::EventLoopBuilder;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc::channel,
};
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder, TrayIconEvent,
};

mod adb;

fn start_browser() {
    open::that_detached("https://app.tangoapp.dev/?desktop=true").unwrap();
}

async fn handle_websocket(ws: WebSocket) {
    let (mut ws_writer, mut ws_reader) = ws.split();
    let (mut adb_reader, mut adb_writer) = adb::connect_or_start().await.unwrap().into_split();

    let (ws_to_adb_sender, mut ws_to_adb_receiver) = channel::<Bytes>(16);
    let (adb_to_ws_sender, mut adb_to_ws_receiver) = channel::<Vec<u8>>(16);

    tokio::join!(
        async move {
            while let Some(Ok(message)) = ws_reader.next().await {
                // Don't merge with `if` above to ignore other message types
                if let Message::Binary(packet) = message {
                    if ws_to_adb_sender.send(packet).await.is_err() {
                        break;
                    }
                }
            }
        },
        async move {
            while let Some(buf) = ws_to_adb_receiver.recv().await {
                if adb_writer.write_all(buf.as_ref()).await.is_err() {
                    break;
                }
            }
            adb_writer.shutdown().await.unwrap();
        },
        async move {
            loop {
                let mut buf = vec![0; 1024 * 1024];
                match adb_reader.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        break;
                    }
                    Ok(n) => {
                        buf.truncate(n);
                        if adb_to_ws_sender.send(buf).await.is_err() {
                            break;
                        }
                    }
                }
            }
        },
        async move {
            while let Some(buf) = adb_to_ws_receiver.recv().await {
                if ws_writer.send(Message::binary(buf)).await.is_err() {
                    break;
                }
            }
            ws_writer.close().await.unwrap();
        }
    );
}

const ARG_AUTO_RUN: &str = "--auto-run";

#[cfg(debug_assertions)]
const PROXY_HOST: &str = "https://tangoapp.dev";
#[cfg(not(debug_assertions))]
const PROXY_HOST: &str = "https://tangoapp.dev";

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn resolve_log_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(mut local_app_data) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            local_app_data.push("Tango Bridge");
            if create_dir_all(&local_app_data).is_ok() {
                return local_app_data.join("tango-bridge.log");
            }
        }
    }

    env::temp_dir().join("tango-bridge.log")
}

fn log_path() -> &'static PathBuf {
    LOG_PATH.get_or_init(resolve_log_path)
}

fn log_message(message: impl AsRef<str>) {
    let message = message.as_ref();
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        if let Ok(since_epoch) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            let _ = write!(
                file,
                "[{}.{:03}] ",
                since_epoch.as_secs(),
                since_epoch.subsec_millis()
            );
        }
        let _ = writeln!(file, "{message}");
    }
}

fn show_error(title: &str, description: &str) {
    log_message(description);
    let _ = MessageDialog::new()
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::Ok)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

fn install_panic_hook() {
    panic::set_hook(Box::new(|panic_info| {
        let mut message = panic_info.to_string();
        if let Some(location) = panic_info.location() {
            message = format!(
                "{message} (at {}:{}:{})",
                location.file(),
                location.line(),
                location.column()
            );
        }

        show_error(
            "Tango Bridge encountered a fatal error",
            &format!(
                "The app crashed and needs to close. Details were written to:\n{}\n\n{message}",
                log_path().display()
            ),
        );
    }));
}

#[axum::debug_handler]
async fn proxy_request(request: Request) -> Result<Response, Response> {
    println!("proxy_request: {} {}", request.method(), request.uri());

    let url = Url::options()
        .base_url(Some(&Url::parse(PROXY_HOST).unwrap()))
        .parse(&request.uri().to_string())
        .map_err(|_| (StatusCode::BAD_REQUEST, "Bad Request").into_response())?;

    let mut headers = request.headers().clone();
    headers.insert("Host", url.host_str().unwrap().parse().unwrap());

    let (client, request) = CLIENT
        .get_or_init(|| reqwest::Client::new())
        .request(request.method().clone(), url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(
            request.into_body().into_data_stream(),
        ))
        .build_split();

    let request = request.map_err(|_| (StatusCode::BAD_REQUEST, "Bad Request").into_response())?;

    let response = client
        .execute(request)
        .await
        .map_err(|_| (StatusCode::BAD_GATEWAY, "Bad Gateway").into_response())?;

    Ok((
        response.status(),
        response.headers().clone(),
        axum::body::Body::new(reqwest::Body::from(response)),
    )
        .into_response())
}

static SINGLE_INSTANCE: OnceLock<single_instance::SingleInstance> = OnceLock::new();

#[tokio::main]
async fn main() {
    install_panic_hook();

    if let Err(err) = run_app().await {
        show_error(
            "Tango Bridge failed to start",
            &format!(
                "{err}\n\nA log file was written to: {}",
                log_path().display()
            ),
        );
    }
}

async fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    log_message("Launching Tango Bridge");

    // macOS app bundle prevents re-launching by default
    #[cfg(not(target_os = "macos"))]
    {
        use single_instance::SingleInstance;

        let single_instance = SINGLE_INSTANCE
            .get_or_try_init(|| SingleInstance::new("tango-bridge-rs"))
            .map_err(|err| format!("Failed to check if app is already running: {err}"))?;

        log_message(format!(
            "single_instance.is_single(): {}",
            single_instance.is_single()
        ));

        if !single_instance.is_single() {
            start_browser();
            return Ok(());
        }
    }

    // Very strangely, running this in `tokio::spawn`
    // will cause `listener` to not stop on Windows
    adb::connect_or_start().await?.shutdown().await?;

    #[cfg(debug_assertions)]
    {
        use tracing::Level;
        use tracing_subscriber::FmtSubscriber;

        let subscriber = FmtSubscriber::builder()
            // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
            // will be written to stdout.
            .with_max_level(Level::TRACE)
            // completes the builder.
            .finish();

        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");
    }

    let app = Router::new()
        .nest(
            "/bridge",
            Router::new()
                .route("/ping", get(|| async { env!("CARGO_PKG_VERSION") }))
                .route(
                    "/",
                    get(|ws: WebSocketUpgrade| async { ws.on_upgrade(handle_websocket) }),
                )
                .route_layer(
                    {
                        let mut allowed_origins: Vec<HeaderValue> = [
                            "http://localhost:3002",
                            "https://tangoapp.dev",
                            "https://app.tangoapp.dev",
                            "https://beta.tangoapp.dev",
                            "https://tunnel.tangoapp.dev",
                        ]
                        .map(|x| x.parse().unwrap())
                        .into();

                        if let Ok(extra_origins) = env::var("TANGO_BRIDGE_ALLOWED_ORIGINS") {
                            for origin in extra_origins.split(',').map(str::trim).filter(|o| !o.is_empty()) {
                                match origin.parse() {
                                    Ok(value) => allowed_origins.push(value),
                                    Err(_) => eprintln!("Ignoring invalid origin in TANGO_BRIDGE_ALLOWED_ORIGINS: {origin}"),
                                }
                            }
                        }

                        CorsLayer::new()
                            .allow_methods([Method::GET, Method::POST])
                            .allow_origin(allowed_origins)
                            .allow_private_network(true)
                    },
                ),
        )
        .fallback(proxy_request);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:15037")
        .await
        .map_err(|err| format!("Failed to bind WebSocket listener: {err}"))?;

    let token = CancellationToken::new();

    let mut server = {
        let token = token.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(token.cancelled_owned())
                .into_future()
                .await
        });
        Some(server)
    };
    log_message(format!(
        "server started on thread {:?}",
        thread::current().id()
    ));

    if env::args().all(|arg| arg != ARG_AUTO_RUN) {
        start_browser()
    }

    let menu_open = MenuItem::new("Open", true, None);

    let auto_launch = AutoLaunchBuilder::new()
        .set_app_name("Tango")
        .set_app_path(env::current_exe()?.to_str().unwrap())
        .set_args(&[ARG_AUTO_RUN])
        .set_use_launch_agent(true)
        .build()
        .map_err(|err| format!("Failed to initialize auto-launch: {err}"))?;
    let auto_launch_enabled = auto_launch
        .is_enabled()
        .map_err(|err| format!("Failed to read auto-launch state: {err}"))?;
    let menu_auto_run = CheckMenuItem::new("Run at startup", true, auto_launch_enabled, None);

    let menu_quit = MenuItem::new("Quit", true, None);

    let tray_menu = Menu::new().map_err(|err| format!("Failed to create tray menu: {err}"))?;
    tray_menu
        .append_items(&[
            &menu_open,
            &menu_auto_run,
            &PredefinedMenuItem::separator(),
            &menu_quit,
        ])
        .map_err(|err| format!("Failed to create tray items: {err}"))?;

    let menu_receiver = MenuEvent::receiver();
    let tray_receiver = TrayIconEvent::receiver();

    let mut tray_icon = None;

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();

    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::EventLoopExtMacOS;

        // https://github.com/glfw/glfw/issues/1552
        event_loop.set_activation_policy(tao::platform::macos::ActivationPolicy::Accessory);
    }

    log_message("Entering event loop");

    event_loop.run(move |event, _, control_flow| {
        *control_flow =
            tao::event_loop::ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16));

        if let tao::event::Event::Reopen { .. } = event {
            start_browser();
            return;
        }

        if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
            match image::load_from_memory_with_format(
                include_bytes!("../tango.png"),
                image::ImageFormat::Png,
            ) {
                Ok(image) => {
                    let image = image.into_rgba8();
                    let (width, height) = image.dimensions();
                    let rgba = image.into_raw();
                    match tray_icon::Icon::from_rgba(rgba, width, height) {
                        Ok(icon) => {
                            tray_icon = TrayIconBuilder::new()
                                .with_tooltip("Tango (rs)")
                                .with_icon(icon)
                                .with_menu(Box::new(tray_menu.clone()))
                                .build()
                                .ok();
                        }
                        Err(err) => {
                            show_error(
                                "Tango Bridge failed to start",
                                &format!("Could not create tray icon: {err}"),
                            );
                            *control_flow = tao::event_loop::ControlFlow::Exit;
                            return;
                        }
                    }
                }
                Err(err) => {
                    show_error(
                        "Tango Bridge failed to start",
                        &format!("Could not load tray icon: {err}"),
                    );
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                    return;
                }
            }

            #[cfg(target_os = "macos")]
            unsafe {
                use core_foundation::runloop::{CFRunLoopGetMain, CFRunLoopWakeUp};

                let rl = CFRunLoopGetMain();
                CFRunLoopWakeUp(rl);
            }
        }

        if let Ok(event) = menu_receiver.try_recv() {
            if event.id == menu_open.id() {
                start_browser();
                return;
            }

            if event.id == menu_auto_run.id() {
                match auto_launch.is_enabled() {
                    Ok(true) => {
                        if let Err(err) = auto_launch.disable() {
                            show_error(
                                "Unable to update startup setting",
                                &format!("Failed to disable auto start: {err}"),
                            );
                        }
                    }
                    Ok(false) => {
                        if let Err(err) = auto_launch.enable() {
                            show_error(
                                "Unable to update startup setting",
                                &format!("Failed to enable auto start: {err}"),
                            );
                        }
                    }
                    Err(err) => {
                        show_error(
                            "Unable to update startup setting",
                            &format!("Failed to read current state: {err}"),
                        );
                    }
                }

                match auto_launch.is_enabled() {
                    Ok(enabled) => menu_auto_run.set_checked(enabled),
                    Err(err) => show_error(
                        "Unable to update startup setting",
                        &format!("Failed to refresh state: {err}"),
                    ),
                }
                return;
            }

            if event.id == menu_quit.id() {
                tray_icon.take();

                token.cancel();
                log_message("trigger token cancel");

                if let Some(server) = server.take() {
                    tokio::task::block_in_place(|| {
                        match tokio::runtime::Handle::current().block_on(server) {
                            Ok(join_result) => match join_result {
                                Ok(_) => log_message("server exited"),
                                Err(err) => log_message(format!("server task error: {err}")),
                            },
                            Err(err) => log_message(format!("Failed to join server task: {err}")),
                        }
                    });
                }

                log_message("exiting main loop");
                *control_flow = tao::event_loop::ControlFlow::Exit;
                return;
            }
        }

        if let Ok(TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Down,
            ..
        }) = tray_receiver.try_recv()
        {
            start_browser();
            return;
        }
    });
}
