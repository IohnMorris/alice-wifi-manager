use crate::net_iface::{AP_IP, WIFI_IFACE};
use dhcproto::v4::{self, DhcpOption, MessageType, Opcode, OptionCode};
use dhcproto::{Decodable, Decoder, Encodable, Encoder};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const POOL_START: u8 = 10;
const POOL_END: u8 = 50;
const LEASE_TIME_SECS: u32 = 3600;

pub struct DhcpServerHandle {
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl DhcpServerHandle {
    pub fn start() -> Result<Self, String> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| format!("DHCP socket create error: {}", e))?;

        let _ = socket.set_reuse_address(true);
        socket
            .set_broadcast(true)
            .map_err(|e| format!("DHCP set_broadcast error: {}", e))?;

        // Привязываем сокет к wlan0, чтобы принимать 255.255.255.255:67
        let _ = socket.bind_device(Some(WIFI_IFACE.as_bytes()));

        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 67);
        socket
            .bind(&SocketAddr::V4(bind_addr).into())
            .map_err(|e| format!("DHCP bind 0.0.0.0:67 error: {}", e))?;

        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .map_err(|e| format!("DHCP set_read_timeout error: {}", e))?;

        let udp: UdpSocket = socket.into();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&stop_flag);

        let join_handle = thread::spawn(move || {
            run_dhcp_loop(udp, flag_clone);
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

impl Drop for DhcpServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_dhcp_loop(udp: UdpSocket, stop_flag: Arc<AtomicBool>) {
    let mut leases: HashMap<Vec<u8>, Ipv4Addr> = HashMap::new();
    let mut next_octet: u8 = POOL_START;
    let mut buf = [0u8; 1500];

    while !stop_flag.load(Ordering::Relaxed) {
        let Ok((n, _src)) = udp.recv_from(&mut buf) else {
            continue;
        };

        let Ok(req) = v4::Message::decode(&mut Decoder::new(&buf[..n])) else {
            continue;
        };

        if req.opcode() != Opcode::BootRequest {
            continue;
        }

        let Some(msg_type) = req.opts().msg_type() else {
            continue;
        };

        let mac = req.chaddr().to_vec();
        if mac.is_empty() {
            continue;
        }

        let assigned_ip = *leases.entry(mac.clone()).or_insert_with(|| {
            let ip = Ipv4Addr::new(192, 168, 4, next_octet);
            next_octet = if next_octet >= POOL_END {
                POOL_START
            } else {
                next_octet + 1
            };
            ip
        });

        let reply_type = match msg_type {
            MessageType::Discover => MessageType::Offer,
            MessageType::Request => {
                // Проверяем, не запрашивает ли клиент IP из чужой старой домашней подсети
                let requested_ip = req
                    .opts()
                    .get(OptionCode::RequestedIpAddress)
                    .and_then(|opt| match opt {
                        DhcpOption::RequestedIpAddress(ip) => Some(*ip),
                        _ => None,
                    })
                    .or_else(|| {
                        let ci = req.ciaddr();
                        if !ci.is_unspecified() {
                            Some(ci)
                        } else {
                            None
                        }
                    });

                if let Some(req_ip) = requested_ip {
                    let octets = req_ip.octets();
                    if octets[0] != 192 || octets[1] != 168 || octets[2] != 4 {
                        MessageType::Nak
                    } else {
                        MessageType::Ack
                    }
                } else {
                    MessageType::Ack
                }
            }
            _ => continue,
        };

        let mut reply = v4::Message::default();
        reply
            .set_opcode(Opcode::BootReply)
            .set_htype(req.htype())
            .set_xid(req.xid())
            .set_flags(req.flags())
            .set_yiaddr(if reply_type == MessageType::Nak {
                Ipv4Addr::UNSPECIFIED
            } else {
                assigned_ip
            })
            .set_siaddr(AP_IP)
            .set_chaddr(&mac);

        reply.opts_mut().insert(DhcpOption::MessageType(reply_type));
        reply.opts_mut().insert(DhcpOption::ServerIdentifier(AP_IP));

        if reply_type != MessageType::Nak {
            reply
                .opts_mut()
                .insert(DhcpOption::AddressLeaseTime(LEASE_TIME_SECS));
            reply
                .opts_mut()
                .insert(DhcpOption::SubnetMask(Ipv4Addr::new(255, 255, 255, 0)));
            reply.opts_mut().insert(DhcpOption::Router(vec![AP_IP]));
            reply
                .opts_mut()
                .insert(DhcpOption::DomainNameServer(vec![AP_IP]));
        }

        let mut out_buf = Vec::with_capacity(512);
        let mut encoder = Encoder::new(&mut out_buf);
        if reply.encode(&mut encoder).is_ok() {
            let bcast_dest = SocketAddrV4::new(Ipv4Addr::BROADCAST, 68);
            let _ = udp.send_to(&out_buf, bcast_dest);
        }
    }
}
