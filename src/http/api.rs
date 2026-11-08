use crate::config_store::{SavedNetworkDto, WpaConfigFile};
use crate::state_machine::{SharedManagerState, TransitionCommand};
use crate::wpa_ctrl::ScannedNetwork;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Serialize)]
pub struct StatusResponse {
    pub mode: String, // "AP", "STA", "TESTING"
    pub primary_ssid: Option<String>,
    pub connected_ssid: Option<String>,
    pub ip_address: Option<String>,
    pub last_error: Option<String>,
    pub last_result: String, // "idle", "testing", "success", "failed"
}

#[derive(Serialize)]
pub struct NetworkItemDto {
    pub ssid: String,
    pub bssid: String,
    pub frequency: u32,
    pub band: String,
    pub signal_dbm: i32,
    pub signal_bars: u8,
    pub security: String,
    pub supported: bool,
    pub is_saved: bool,
    pub is_primary: bool,
    pub available: bool,
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub ssid: String,
    pub password: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Deserialize)]
pub struct SaveNetworkRequest {
    pub ssid: String,
    pub password: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub make_primary: bool,
}

#[derive(Deserialize)]
pub struct SetPrimaryRequest {
    pub id: usize,
}

pub struct ApiResult {
    pub status_code: u16,
    pub json_body: String,
    pub post_action: Option<TransitionCommand>,
}

impl ApiResult {
    fn ok<T: Serialize>(payload: &T) -> Self {
        Self {
            status_code: 200,
            json_body: serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string()),
            post_action: None,
        }
    }

    fn ok_with_action<T: Serialize>(payload: &T, action: TransitionCommand) -> Self {
        Self {
            status_code: 200,
            json_body: serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string()),
            post_action: Some(action),
        }
    }

    fn err(status_code: u16, msg: &str) -> Self {
        let body = serde_json::json!({
            "ok": false,
            "error": msg
        });
        Self {
            status_code,
            json_body: body.to_string(),
            post_action: None,
        }
    }
}

/// Обрабатывает запросы к `/api/*` под защитой мьютекса состояния.
pub fn handle_api_request(
    method: &str,
    path: &str,
    body: &str,
    state: &Arc<Mutex<SharedManagerState>>,
) -> ApiResult {
    let clean_path = path.split('?').next().unwrap_or(path);

    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    match (method, clean_path) {
        ("GET", "/api/status") => {
            let cfg = WpaConfigFile::load().ok();
            let primary_ssid = cfg
                .as_ref()
                .and_then(|c| c.get_primary().map(|n| n.ssid.clone()));

            let resp = StatusResponse {
                mode: guard.mode_str().to_string(),
                primary_ssid,
                connected_ssid: guard.connected_ssid.clone(),
                ip_address: guard.current_ip.clone(),
                last_error: guard.last_error.clone(),
                last_result: guard.last_result.clone(),
            };
            ApiResult::ok(&resp)
        }

        ("GET", "/api/networks") => {
            let cfg = WpaConfigFile::load().ok();
            let mut items: Vec<NetworkItemDto> = guard
                .scan_cache
                .iter()
                .map(|s: &ScannedNetwork| {
                    let stored = cfg.as_ref().and_then(|c| c.find_by_ssid(&s.ssid));
                    NetworkItemDto {
                        ssid: s.ssid.clone(),
                        bssid: s.bssid.clone(),
                        frequency: s.frequency,
                        band: s.band.clone(),
                        signal_dbm: s.signal_dbm,
                        signal_bars: s.signal_bars,
                        security: s.security.clone(),
                        supported: s.supported,
                        is_saved: stored.is_some(),
                        is_primary: stored.map(|n| n.is_primary).unwrap_or(false),
                        available: true,
                    }
                })
                .collect();

            if let Some(config) = cfg.as_ref() {
                for saved in &config.networks {
                    if items.iter().any(|item| item.ssid == saved.ssid) {
                        continue;
                    }

                    let is_open = saved.key_mgmt.eq_ignore_ascii_case("NONE");
                    items.push(NetworkItemDto {
                        ssid: saved.ssid.clone(),
                        bssid: String::new(),
                        frequency: 0,
                        band: String::new(),
                        signal_dbm: 0,
                        signal_bars: 0,
                        security: if is_open {
                            "Открытая".to_string()
                        } else {
                            saved.key_mgmt.clone()
                        },
                        supported: !saved.key_mgmt.contains("EAP"),
                        is_saved: true,
                        is_primary: saved.is_primary,
                        available: false,
                    });
                }
            }

            ApiResult::ok(&serde_json::json!({
                "ok": true,
                "networks": items
            }))
        }

        ("POST", "/api/scan") => {
            if guard.busy_transaction {
                return ApiResult::err(409, "Выполняется другая сетевая операция");
            }
            if guard.mode == crate::state_machine::OperatingMode::AccessPoint {
                return ApiResult::ok(&serde_json::json!({
                    "ok": true,
                    "refreshed": false,
                    "source": "pre_ap_cache",
                    "count": guard.scan_cache.len(),
                    "message": "Сканирование временно недоступно в режиме точки доступа. Показан список, полученный до запуска AP."
                }));
            }

            let fresh = match crate::wpa_ctrl::scan_networks() {
                Ok(networks) => networks,
                Err(error) => {
                    crate::wpa_ctrl::append_debug_log(&format!("Live scan failed: {}", error));
                    return ApiResult::err(
                        503,
                        &format!("Не удалось просканировать эфир: {}", error),
                    );
                }
            };
            guard.scan_cache = fresh;
            ApiResult::ok(&serde_json::json!({
                "ok": true,
                "refreshed": true,
                "source": "live_scan",
                "count": guard.scan_cache.len()
            }))
        }

        ("GET", "/api/saved-networks") => match WpaConfigFile::load() {
            Ok(cfg) => {
                let saved: Vec<SavedNetworkDto> = cfg.to_saved_dtos();
                ApiResult::ok(&serde_json::json!({
                    "ok": true,
                    "networks": saved
                }))
            }
            Err(e) => ApiResult::err(500, &format!("Ошибка чтения конфига: {}", e)),
        },

        ("POST", "/api/networks") => {
            if guard.busy_transaction {
                return ApiResult::err(409, "Выполняется проверка подключения");
            }
            let Ok(req) = serde_json::from_str::<SaveNetworkRequest>(body) else {
                return ApiResult::err(400, "Некорректный формат JSON");
            };
            let ssid = req.ssid.trim();
            if ssid.is_empty() || ssid.len() > 32 {
                return ApiResult::err(400, "Некорректная длина SSID (1..32 символа)");
            }
            if let Some(ref pwd) = req.password {
                if !pwd.is_empty() && (pwd.len() < 8 || pwd.len() > 63) {
                    return ApiResult::err(400, "Пароль WPA2 должен содержать от 8 до 63 символов");
                }
            }

            let mut cfg = match WpaConfigFile::load() {
                Ok(c) => c,
                Err(e) => return ApiResult::err(500, &e),
            };

            // Если это единственная сеть в конфиге, делаем её основной
            let make_primary = req.make_primary || cfg.networks.is_empty();
            cfg.upsert_network(ssid, req.password.as_deref(), req.hidden, make_primary);
            if let Err(e) = cfg.save_atomic() {
                crate::wpa_ctrl::append_debug_log(&format!(
                    "Failed to persist manually added network {}: {}",
                    ssid, e
                ));
                return ApiResult::err(500, &e);
            }
            if guard.mode == crate::state_machine::OperatingMode::Station {
                if let Err(error) = crate::wpa_ctrl::reconfigure_saved_config() {
                    crate::wpa_ctrl::append_debug_log(&format!(
                        "Saved network persisted but STA runtime reconfigure failed: {}",
                        error
                    ));
                }
            }
            crate::wpa_ctrl::append_debug_log(&format!(
                "Persisted network profile {} via POST /api/networks",
                ssid
            ));

            ApiResult::ok(&serde_json::json!({
                "ok": true,
                "networks": cfg.to_saved_dtos()
            }))
        }

        ("POST", "/api/connect") => {
            if guard.busy_transaction {
                return ApiResult::err(409, "Проверка подключения уже запущена");
            }
            let Ok(req) = serde_json::from_str::<ConnectRequest>(body) else {
                return ApiResult::err(400, "Некорректный формат запроса");
            };
            let ssid = req.ssid.trim().to_string();
            if ssid.is_empty() || ssid.len() > 32 {
                return ApiResult::err(400, "Укажите корректное название сети (SSID)");
            }

            let cfg = match WpaConfigFile::load() {
                Ok(c) => c,
                Err(e) => return ApiResult::err(500, &e),
            };

            // Определяем пароль: либо передан новый, либо берём из уже сохранённого профиля
            let resolved_psk = match req.password {
                Some(ref p) if !p.is_empty() => {
                    if p.len() < 8 || p.len() > 63 {
                        return ApiResult::err(
                            400,
                            "Пароль Wi-Fi должен содержать от 8 до 63 символов",
                        );
                    }
                    Some(p.clone())
                }
                _ => cfg
                    .find_by_ssid(&ssid)
                    .and_then(crate::config_store::extract_clean_psk),
            };

            guard.busy_transaction = true;
            guard.last_error = None;
            guard.last_result = "testing".to_string();

            // Сначала возвращаем браузеру ответ с таймером 20 секунд (Этап 1 п. 2),
            // после чего конечный автомат переключит wlan0 для проверки новой сети.
            ApiResult::ok_with_action(
                &serde_json::json!({
                    "ok": true,
                    "status": "testing",
                    "timeout_sec": 20,
                    "ssid": ssid
                }),
                TransitionCommand::TestAndConnect {
                    ssid,
                    psk: resolved_psk,
                    is_hidden: req.hidden,
                },
            )
        }

        ("PUT", "/api/networks/primary") => {
            if guard.busy_transaction {
                return ApiResult::err(409, "Проверка подключения уже запущена");
            }
            let Ok(req) = serde_json::from_str::<SetPrimaryRequest>(body) else {
                return ApiResult::err(400, "Некорректный ID сети");
            };

            let cfg = match WpaConfigFile::load() {
                Ok(c) => c,
                Err(e) => return ApiResult::err(500, &e),
            };

            let Some(stored) = cfg.find_by_id(req.id) else {
                return ApiResult::err(404, "Сохранённая сеть не найдена");
            };

            let ssid = stored.ssid.clone();
            let psk = crate::config_store::extract_clean_psk(stored);
            let is_hidden = stored.scan_ssid;

            guard.busy_transaction = true;
            guard.last_error = None;
            guard.last_result = "testing".to_string();

            ApiResult::ok_with_action(
                &serde_json::json!({
                    "ok": true,
                    "status": "testing",
                    "timeout_sec": 20,
                    "ssid": ssid
                }),
                TransitionCommand::TestAndConnect {
                    ssid,
                    psk,
                    is_hidden,
                },
            )
        }

        ("POST", "/api/setup/stop") => {
            if guard.busy_transaction {
                return ApiResult::err(409, "Выполняется транзакция подключения");
            }
            ApiResult::ok_with_action(
                &serde_json::json!({
                    "ok": true,
                    "status": "stopping_ap"
                }),
                TransitionCommand::StopApAndReturnToPrimary,
            )
        }

        _ => {
            if method == "DELETE" && clean_path.starts_with("/api/networks/") {
                if guard.busy_transaction {
                    return ApiResult::err(409, "Выполняется транзакция подключения");
                }

                let id_str = clean_path.trim_start_matches("/api/networks/").trim();
                let Ok(target_id) = id_str.parse::<usize>() else {
                    return ApiResult::err(400, "Некорректный идентификатор сети");
                };

                let mut cfg = match WpaConfigFile::load() {
                    Ok(c) => c,
                    Err(e) => return ApiResult::err(500, &e),
                };

                if let Err(e) = cfg.remove_by_id(target_id) {
                    return ApiResult::err(404, &e);
                }
                if let Err(e) = cfg.save_atomic() {
                    crate::wpa_ctrl::append_debug_log(&format!(
                        "Failed to persist deletion of network id {}: {}",
                        target_id, e
                    ));
                    return ApiResult::err(500, &e);
                }
                if guard.mode == crate::state_machine::OperatingMode::Station {
                    if let Err(error) = crate::wpa_ctrl::reconfigure_saved_config() {
                        crate::wpa_ctrl::append_debug_log(&format!(
                            "Network deletion persisted but STA runtime reconfigure failed: {}",
                            error
                        ));
                    }
                }
                crate::wpa_ctrl::append_debug_log(&format!(
                    "Persisted deletion of network id {} via DELETE /api/networks",
                    target_id
                ));

                return ApiResult::ok(&serde_json::json!({
                    "ok": true,
                    "networks": cfg.to_saved_dtos()
                }));
            }

            ApiResult::err(404, "API маршрут не найден")
        }
    }
}
