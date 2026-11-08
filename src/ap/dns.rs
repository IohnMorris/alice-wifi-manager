use crate::net_iface::AP_IP;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct DnsServerHandle {
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl DnsServerHandle {
    pub fn start() -> Result<Self, String> {
        let bind_addr = SocketAddrV4::new(AP_IP, 53);
        crate::wpa_ctrl::append_debug_log(&format!(
            "DNS start: binding IPv4-only socket to {}",
            bind_addr
        ));

        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(|e| {
            let detail = describe_port_53();
            format!("DNS socket create failed: {}; {}", e, detail)
        })?;
        socket
            .set_reuse_address(true)
            .map_err(|e| format!("DNS SO_REUSEADDR setup failed: {}", e))?;
        socket
            .bind(&SocketAddr::V4(bind_addr).into())
            .map_err(|e| {
                let detail = describe_port_53();
                format!(
                    "DNS bind failed on IPv4 address {} (not 0.0.0.0/[::]): {}; {}",
                    bind_addr, e, detail
                )
            })?;
        let udp: UdpSocket = socket.into();

        udp.set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|e| format!("DNS set_read_timeout error: {}", e))?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&stop_flag);

        let join_handle = thread::spawn(move || {
            run_dns_loop(udp, flag_clone);
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

fn describe_port_53() -> String {
    let mut details = Vec::new();
    for path in [
        "/proc/net/udp",
        "/proc/net/udp6",
        "/proc/net/tcp",
        "/proc/net/tcp6",
    ] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let Some(local) = fields.get(1) else {
                    continue;
                };
                if !local.ends_with(":0035") {
                    continue;
                }
                let inode = fields.get(9).copied().unwrap_or("unknown");
                details.push(format!("{} local={} inode={}", path, local, inode));
            }
        }
    }
    if details.is_empty() {
        "No port-53 entries found in /proc/net".to_string()
    } else {
        details.join("; ")
    }
}

impl Drop for DnsServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_dns_loop(udp: UdpSocket, stop_flag: Arc<AtomicBool>) {
    let mut buf = [0u8; 512];

    while !stop_flag.load(Ordering::Relaxed) {
        let Ok((n, src)) = udp.recv_from(&mut buf) else {
            continue;
        };

        if let Some(resp) = build_dns_spoof_response(&buf[..n]) {
            let _ = udp.send_to(&resp, src);
        }
    }
}

/// Формирует DNS-ответ: на запросы типа A (1) и ANY (255) возвращает 192.168.4.1,
/// на AAAA (28) и прочие возвращает пустой успешный ответ (NOERROR, ANCOUNT=0).
fn build_dns_spoof_response(pkt: &[u8]) -> Option<Vec<u8>> {
    if pkt.len() < 12 {
        return None;
    }

    // Проверяем, что это стандартный запрос (QR == 0) и есть хотя бы 1 вопрос
    if (pkt[2] & 0x80) != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([pkt[4], pkt[5]]);
    if qdcount == 0 {
        return None;
    }

    // Проходим по доменному имени в первой секции Question
    let mut pos = 12;
    while pos < pkt.len() {
        let label_len = pkt[pos] as usize;
        if label_len == 0 {
            pos += 1;
            break;
        }
        if (label_len & 0xC0) == 0xC0 {
            pos += 2;
            break;
        }
        pos += 1 + label_len;
    }

    // После имени должны идти QTYPE (2 байта) и QCLASS (2 байта)
    if pos + 4 > pkt.len() {
        return None;
    }

    let qtype = u16::from_be_bytes([pkt[pos], pkt[pos + 1]]);
    let question_end = pos + 4;
    let is_a_query = qtype == 1 || qtype == 255;

    let mut resp = Vec::with_capacity(question_end + 16);
    // Transaction ID
    resp.push(pkt[0]);
    resp.push(pkt[1]);
    // Flags: QR=1, AA=1, RD=1, RA=1, RCODE=0 (0x8580)
    resp.extend_from_slice(&[0x85, 0x80]);
    // QDCOUNT = 1
    resp.extend_from_slice(&[0x00, 0x01]);
    // ANCOUNT = 1 для A-запроса, 0 для остальных (например, IPv6 AAAA)
    if is_a_query {
        resp.extend_from_slice(&[0x00, 0x01]);
    } else {
        resp.extend_from_slice(&[0x00, 0x00]);
    }
    // NSCOUNT = 0, ARCOUNT = 0
    resp.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

    // Копируем секцию Question целиком
    resp.extend_from_slice(&pkt[12..question_end]);

    if is_a_query {
        // Ссылка на имя в Question (0xC00C)
        resp.extend_from_slice(&[0xC0, 0x0C]);
        // TYPE A (1), CLASS IN (1)
        resp.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        // TTL = 60 секунд
        resp.extend_from_slice(&60u32.to_be_bytes());
        // RDLENGTH = 4, RDATA = 192.168.4.1
        resp.extend_from_slice(&[0x00, 0x04]);
        resp.extend_from_slice(&AP_IP.octets());
    }

    Some(resp)
}
