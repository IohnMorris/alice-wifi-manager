pub mod api;
pub mod captive;

use crate::net_iface::AP_IP;
use crate::state_machine::{SharedManagerState, TransitionCommand};
use std::io::{Read, Write};
use std::net::{SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const EMBEDDED_INDEX_HTML: &str = include_str!("../../web/index.html");
const MAX_HTTP_CLIENTS: usize = 8;
const MAX_HTTP_REQUEST_BYTES: usize = 8192;
static ACTIVE_HTTP_CLIENTS: AtomicUsize = AtomicUsize::new(0);

pub struct HttpServerHandle {
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl HttpServerHandle {
    pub fn start(
        state: Arc<Mutex<SharedManagerState>>,
        cmd_tx: SyncSender<TransitionCommand>,
    ) -> Result<Self, String> {
        let bind_addr = SocketAddrV4::new(AP_IP, 80);
        let listener = TcpListener::bind(bind_addr)
            .map_err(|e| format!("Failed to bind HTTP server on 192.168.4.1:80: {}", e))?;

        listener
            .set_nonblocking(true)
            .map_err(|e| format!("HTTP set_nonblocking error: {}", e))?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&stop_flag);

        let join_handle = thread::spawn(move || {
            run_http_loop(listener, state, cmd_tx, flag_clone);
        });

        Ok(Self {
            stop_flag,
            join_handle: Some(join_handle),
        })
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for HttpServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_http_loop(
    listener: TcpListener,
    state: Arc<Mutex<SharedManagerState>>,
    cmd_tx: SyncSender<TransitionCommand>,
    stop_flag: Arc<AtomicBool>,
) {
    while !stop_flag.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                if ACTIVE_HTTP_CLIENTS.fetch_add(1, Ordering::AcqRel) >= MAX_HTTP_CLIENTS {
                    ACTIVE_HTTP_CLIENTS.fetch_sub(1, Ordering::Release);
                    continue;
                }
                let state_ref = Arc::clone(&state);
                let tx_ref = cmd_tx.clone();
                thread::spawn(move || {
                    handle_client(stream, state_ref, tx_ref);
                    ACTIVE_HTTP_CLIENTS.fetch_sub(1, Ordering::Release);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(40));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_client(
    mut stream: TcpStream,
    state: Arc<Mutex<SharedManagerState>>,
    cmd_tx: SyncSender<TransitionCommand>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));

    let mut buf = [0u8; MAX_HTTP_REQUEST_BYTES];
    let Ok(n) = stream.read(&mut buf) else {
        return;
    };
    if n == 0 {
        return;
    }

    let raw_req = String::from_utf8_lossy(&buf[..n]);
    let Some((headers_part, body_part)) = raw_req.split_once("\r\n\r\n") else {
        return;
    };
    if n == MAX_HTTP_REQUEST_BYTES {
        crate::wpa_ctrl::append_debug_log("HTTP request rejected: request exceeds bounded buffer");
        return;
    }

    let mut lines = headers_part.lines();
    let Some(request_line) = lines.next() else {
        return;
    };

    let mut req_parts = request_line.split_whitespace();
    let method = req_parts.next().unwrap_or("GET");
    let path = req_parts.next().unwrap_or("/");

    let mut host_header: Option<String> = None;
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            let value = v.trim();
            if key.eq_ignore_ascii_case("host") {
                host_header = Some(value.to_string());
            } else if key.eq_ignore_ascii_case("content-length") {
                let Ok(length) = value.parse::<usize>() else {
                    return;
                };
                if length > 4096 {
                    crate::wpa_ctrl::append_debug_log("HTTP request rejected: body exceeds limit");
                    return;
                }
                content_length = length;
            }
        }
    }
    if body_part.len() > 4096 || content_length > body_part.len() {
        crate::wpa_ctrl::append_debug_log("HTTP request rejected: incomplete or oversized body");
        return;
    }

    // 1. Обработка системных проверок Captive Portal (HTTP 302 -> http://192.168.4.1/)
    if captive::is_captive_probe(path, host_header.as_deref()) {
        let resp = captive::build_captive_redirect_response();
        let _ = stream.write_all(&resp);
        let _ = stream.flush();
        return;
    }

    // 2. Обработка маршрутов JSON API (/api/*)
    if path.starts_with("/api/") {
        let api_res = api::handle_api_request(method, path, body_part, &state);
        let status_text = match api_res.status_code {
            200 => "200 OK",
            400 => "400 Bad Request",
            404 => "404 Not Found",
            409 => "409 Conflict",
            503 => "503 Service Unavailable",
            _ => "500 Internal Server Error",
        };

        let response = format!(
            "HTTP/1.1 {}\r\n\
             Content-Type: application/json; charset=utf-8\r\n\
             Cache-Control: no-store, no-cache, must-revalidate\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            status_text,
            api_res.json_body.len(),
            api_res.json_body
        );

        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Both);

        // Если API-запрос инициировал проверку подключения или остановку AP,
        // даём браузеру 300 мс гарантированно получить HTTP-ответ до сброса wlan0
        if let Some(cmd) = api_res.post_action {
            thread::sleep(Duration::from_millis(300));
            let _ = cmd_tx.try_send(cmd);
        }
        return;
    }

    // 3. Раздача встроенного SPA веб-интерфейса для "/", "/index.html" и любых прочих GET-путей
    let html_bytes = EMBEDDED_INDEX_HTML.as_bytes();
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Cache-Control: no-store, no-cache, must-revalidate\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        html_bytes.len()
    );

    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(html_bytes);
    let _ = stream.flush();
}
