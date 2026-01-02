#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env,
    future::IntoFuture,
    io,
    net::SocketAddr,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use auto_launch::AutoLaunchBuilder;
use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket},
        Request, State, WebSocketUpgrade,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ctrlc;
use futures_util::{SinkExt, StreamExt};
use http::{Method, StatusCode};
use reqwest::Url;
use serde::Deserialize;
use socket2::{Domain, Protocol, Socket, Type};
use tao::event_loop::EventLoopBuilder;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
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

async fn bridge_ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(state, socket))
}

#[derive(Clone)]
struct AppState {
    shutdown: CancellationToken,
}

async fn handle_websocket(state: AppState, ws: WebSocket) {
    let shutdown = state.shutdown.clone();

    let (mut ws_writer, mut ws_reader) = ws.split();
    let (mut adb_reader, mut adb_writer) = adb::connect_or_start().await.unwrap().into_split();

    let (ws_to_adb_sender, mut ws_to_adb_receiver) = channel::<Bytes>(16);
    let (adb_to_ws_sender, mut adb_to_ws_receiver) = channel::<Vec<u8>>(16);

    let shutdown_ws_reader = shutdown.clone();
    let shutdown_adb_writer = shutdown.clone();
    let shutdown_adb_reader = shutdown.clone();
    let shutdown_ws_writer = shutdown;

    tokio::join!(
        async move {
            loop {
                tokio::select! {
                    _ = shutdown_ws_reader.cancelled() => {
                        break;
                    }
                    maybe_message = ws_reader.next() => {
                        if let Some(Ok(message)) = maybe_message {
                            if let Message::Binary(packet) = message {
                                if ws_to_adb_sender.send(packet).await.is_err() {
                                    break;
                                }
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        },
        async move {
            loop {
                tokio::select! {
                    _ = shutdown_adb_writer.cancelled() => {
                        break;
                    }
                    Some(buf) = ws_to_adb_receiver.recv() => {
                        if adb_writer.write_all(buf.as_ref()).await.is_err() {
                            break;
                        }
                    }
                    else => break,
                }
            }
            if let Err(err) = adb_writer.shutdown().await {
                eprintln!("Failed to shutdown ADB writer cleanly: {err}");
            }
        },
        async move {
            loop {
                let mut buf = vec![0; 1024 * 1024];
                tokio::select! {
                    _ = shutdown_adb_reader.cancelled() => {
                        break;
                    }
                    read_result = adb_reader.read(&mut buf) => {
                        match read_result {
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
                }
            }
        },
        async move {
            loop {
                tokio::select! {
                    _ = shutdown_ws_writer.cancelled() => {
                        break;
                    }
                    Some(buf) = adb_to_ws_receiver.recv() => {
                        if ws_writer.send(Message::binary(buf)).await.is_err() {
                            break;
                        }
                    }
                    else => break,
                }
            }
            if let Err(err) = ws_writer.close().await {
                eprintln!("Failed to close websocket cleanly: {err}");
            }
        }
    );
}

#[derive(Deserialize)]
struct CloudflaredInstallRequest {
    token: String,
}

#[axum::debug_handler]
async fn install_cloudflared(
    Json(payload): Json<CloudflaredInstallRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let exe_path =
        env::current_exe().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let cloudflared_path = exe_path.with_file_name("cloudflared.exe");

    if !cloudflared_path.exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "cloudflared.exe không nằm cạnh thực thi: {}",
                cloudflared_path.display()
            ),
        ));
    }

    let output = Command::new(&cloudflared_path)
        .args(["service", "install", &payload.token])
        .output()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(stdout)
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

const ARG_AUTO_RUN: &str = "--auto-run";

#[cfg(debug_assertions)]
const PROXY_HOST: &str = "https://tangoapp.dev";
#[cfg(not(debug_assertions))]
const PROXY_HOST: &str = "https://tangoapp.dev";

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn bind_reusable_listener(addr: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;

    tokio::net::TcpListener::from_std(socket.into())
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
    // macOS app bundle prevents re-launching by default
    #[cfg(not(target_os = "macos"))]
    {
        use single_instance::SingleInstance;

        let single_instance =
            SINGLE_INSTANCE.get_or_init(|| SingleInstance::new("tango-bridge-rs").unwrap());
        println!(
            "single_instance.is_single(): {}",
            single_instance.is_single()
        );
        if !single_instance.is_single() {
            start_browser();
            return;
        }
    }

    // Very strangely, running this in `tokio::spawn`
    // will cause `listener` to not stop on Windows
    adb::connect_or_start()
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();

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

    let shutdown_token = CancellationToken::new();
    let state = AppState {
        shutdown: shutdown_token.clone(),
    };

    let app = Router::new()
        .nest(
            "/bridge",
            Router::new()
                .route("/ping", get(|| async { env!("CARGO_PKG_VERSION") }))
                .route("/", get(bridge_ws_handler))
                .route_layer(
                    CorsLayer::new()
                        .allow_methods([Method::GET, Method::POST])
                        .allow_origin(
                            [
                                "http://localhost:3002",
                                "http://localhost:8000",
                                "https://app.hoadev.online",
                            ]
                            .map(|x| x.parse().unwrap()),
                        )
                        .allow_private_network(true),
                )
                .with_state(state.clone()),
        )
        .route("/cloudflared/install", post(install_cloudflared))
        .fallback(proxy_request)
        .with_state(state.clone());

    let addr: SocketAddr = "0.0.0.0:15038".parse().unwrap();
    let listener = match bind_reusable_listener(addr) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("Failed to bind listener at {addr}: {err}");
            return;
        }
    };

    let mut server = {
        let token = shutdown_token.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(token.cancelled_owned())
                .into_future()
                .await
        });
        Some(server)
    };
    println!("server started on thread {:?}", thread::current().id());

    if env::args().all(|arg| arg != ARG_AUTO_RUN) {
        start_browser()
    }

    let menu_open = MenuItem::new("Open", true, None);

    let auto_launch = AutoLaunchBuilder::new()
        .set_app_name("Tango")
        .set_app_path(env::current_exe().unwrap().to_str().unwrap())
        .set_args(&[ARG_AUTO_RUN])
        .set_use_launch_agent(true)
        .build()
        .unwrap();
    let menu_auto_run = CheckMenuItem::new(
        "Run at startup",
        true,
        auto_launch.is_enabled().unwrap(),
        None,
    );

    let menu_quit = MenuItem::new("Quit", true, None);

    let tray_menu = Menu::new();
    tray_menu
        .append_items(&[
            &menu_open,
            &menu_auto_run,
            &PredefinedMenuItem::separator(),
            &menu_quit,
        ])
        .unwrap();

    let menu_receiver = MenuEvent::receiver();
    let tray_receiver = TrayIconEvent::receiver();

    let mut tray_icon = None;

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();
    let event_loop_proxy = event_loop.create_proxy();

    let shutdown_for_signal = shutdown_token.clone();
    let proxy_for_signal = event_loop_proxy.clone();
    ctrlc::set_handler(move || {
        println!("Shutting down gracefully (Ctrl+C)...");
        shutdown_for_signal.cancel();
        if let Err(err) = proxy_for_signal.send_event(()) {
            eprintln!("Failed to signal event loop shutdown: {err}");
        }
    })
    .expect("Error setting Ctrl-C handler");

    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::EventLoopExtMacOS;

        // https://github.com/glfw/glfw/issues/1552
        event_loop.set_activation_policy(tao::platform::macos::ActivationPolicy::Accessory);
    }

    println!("before main loop");

    event_loop.run(move |event, _, control_flow| {
        *control_flow =
            tao::event_loop::ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16));

        if let tao::event::Event::UserEvent(()) = event {
            tray_icon.take();

            shutdown_token.cancel();

            if let Some(server) = server.take() {
                tokio::task::block_in_place(|| {
                    match tokio::runtime::Handle::current().block_on(server) {
                        Ok(Ok(())) => println!("server exited"),
                        Ok(Err(err)) => eprintln!("server exited with error: {err}"),
                        Err(join_err) => eprintln!("failed to join server task: {join_err}"),
                    }
                });
            }

            println!("exiting main loop");
            *control_flow = tao::event_loop::ControlFlow::Exit;
            return;
        }

        if let tao::event::Event::Reopen { .. } = event {
            start_browser();
            return;
        }

        if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
            let image = image::load_from_memory_with_format(
                include_bytes!("../tango.png"),
                image::ImageFormat::Png,
            )
            .unwrap()
            .into_rgba8();
            let (width, height) = image.dimensions();
            let rgba = image.into_raw();
            let icon = tray_icon::Icon::from_rgba(rgba, width, height).unwrap();

            tray_icon = Some(
                TrayIconBuilder::new()
                    .with_tooltip("Tango (rs)")
                    .with_icon(icon)
                    .with_menu(Box::new(tray_menu.clone()))
                    .build()
                    .unwrap(),
            );

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
                if auto_launch.is_enabled().unwrap() {
                    auto_launch.disable().unwrap();
                } else {
                    auto_launch.enable().unwrap();
                }
                menu_auto_run.set_checked(auto_launch.is_enabled().unwrap());
                return;
            }

            if event.id == menu_quit.id() {
                tray_icon.take();

                shutdown_token.cancel();
                println!("trigger token cancel");

                if let Some(server) = server.take() {
                    tokio::task::block_in_place(|| {
                        match tokio::runtime::Handle::current().block_on(server) {
                            Ok(Ok(())) => println!("server exited"),
                            Ok(Err(err)) => eprintln!("server exited with error: {err}"),
                            Err(join_err) => eprintln!("failed to join server task: {join_err}"),
                        }
                    });
                }

                println!("exiting main loop");
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
