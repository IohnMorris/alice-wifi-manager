use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const WPA_CONF_PATH: &str = "/bkupgrade/data/wpa_supplicant.conf";
pub const WPA_CONF_BAK_PATH: &str = "/bkupgrade/data/wpa_supplicant.conf.bak";
pub const WPA_CONF_TMP_PATH: &str = "/bkupgrade/data/wpa_supplicant.conf.tmp";

const DEFAULT_HEADER: &str = "ctrl_interface=/var/run/wpa_supplicant\nupdate_config=1";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_NETWORKS: usize = 64;
const MAX_CONFIG_LINE_BYTES: usize = 1024;

/// Внутреннее представление профиля сети из wpa_supplicant.conf (с паролем и доп. полями).
#[derive(Debug, Clone)]
pub struct StoredNetwork {
    pub id: usize,
    pub ssid: String,
    pub key_mgmt: String,
    pub psk: Option<String>,
    pub is_primary: bool,
    pub scan_ssid: bool,
    pub extra_lines: Vec<String>,
}

/// Публичное представление сохранённой сети для Web UI и JSON API (строго без паролей).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedNetworkDto {
    pub id: usize,
    pub ssid: String,
    pub key_mgmt: String,
    pub is_primary: bool,
}

#[derive(Debug, Clone)]
pub struct WpaConfigFile {
    pub header_lines: Vec<String>,
    pub networks: Vec<StoredNetwork>,
}

impl WpaConfigFile {
    ///Проверяет целостность основного конфига при старте и восстанавливает его из `.bak` при сбое питания.
    pub fn ensure_valid_or_restore() -> Result<(), String> {
        let main_valid = Self::is_file_structurally_valid(WPA_CONF_PATH);
        if main_valid {
            let backup_valid = Self::is_file_structurally_valid(WPA_CONF_BAK_PATH);
            if !backup_valid {
                // Резервная копия создаётся только один раз, если её ещё нет.
                let _ = Self::backup_current();
            } else {
                let main = Self::read_bounded(WPA_CONF_PATH).unwrap_or_default();
                let backup = Self::read_bounded(WPA_CONF_BAK_PATH).unwrap_or_default();
                if main != backup {
                    // .bak содержит последнюю подтверждённую версию. Основной
                    // файл может быть переписан старым runtime-состоянием
                    // wpa_supplicant, поэтому не принимаем его автоматически.
                    fs::copy(WPA_CONF_BAK_PATH, WPA_CONF_PATH)
                        .map_err(|e| format!("Failed to restore committed config: {}", e))?;
                    let _ = fs::set_permissions(WPA_CONF_PATH, fs::Permissions::from_mode(0o600));
                }
            }
            return Ok(());
        }

        if Self::is_file_structurally_valid(WPA_CONF_BAK_PATH) {
            fs::copy(WPA_CONF_BAK_PATH, WPA_CONF_PATH)
                .map_err(|e| format!("Failed to restore config from .bak: {}", e))?;
            let _ = fs::set_permissions(WPA_CONF_PATH, fs::Permissions::from_mode(0o600));
            return Ok(());
        }

        // Если ни основного, ни резервного файла нет — инициализируем чистый штатный заголовок
        let fresh = Self {
            header_lines: DEFAULT_HEADER.lines().map(String::from).collect(),
            networks: Vec::new(),
        };
        fresh.save_atomic()
    }

    fn read_bounded(path: &str) -> Result<String, String> {
        let file = fs::File::open(path).map_err(|e| format!("Failed to open {}: {}", path, e))?;
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Failed to read {}: {}", path, e))?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(format!("Wi-Fi config exceeds {} bytes", MAX_CONFIG_BYTES));
        }
        String::from_utf8(bytes).map_err(|e| format!("Wi-Fi config is not valid UTF-8: {}", e))
    }

    fn is_file_structurally_valid(path: &str) -> bool {
        let Ok(content) = Self::read_bounded(path) else {
            return false;
        };
        let trimmed = content.trim();
        if trimmed.is_empty() || !trimmed.contains("ctrl_interface=") {
            return false;
        }
        let open_braces =
            trimmed.matches("network={").count() + trimmed.matches("network = {").count();
        let close_braces = trimmed.lines().filter(|l| l.trim() == "}").count();
        open_braces == close_braces
    }

    /// Создаёт резервную копию `/bkupgrade/data/wpa_supplicant.conf.bak`.
    pub fn backup_current() -> Result<(), String> {
        match fs::copy(WPA_CONF_PATH, WPA_CONF_BAK_PATH) {
            Ok(_) => {
                let _ = fs::set_permissions(WPA_CONF_BAK_PATH, fs::Permissions::from_mode(0o600));
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Failed to create .bak copy: {}", error)),
        }
    }

    /// Восстанавливает рабочую конфигурацию из `.bak` при откате транзакции.
    pub fn restore_from_backup() -> Result<(), String> {
        match fs::copy(WPA_CONF_BAK_PATH, WPA_CONF_PATH) {
            Ok(_) => {
                let _ = fs::set_permissions(WPA_CONF_PATH, fs::Permissions::from_mode(0o600));
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Failed to restore from .bak: {}", error)),
        }
    }

    /// Парсит `/bkupgrade/data/wpa_supplicant.conf`.
    pub fn load() -> Result<Self, String> {
        Self::ensure_valid_or_restore()?;
        let content = Self::read_bounded(WPA_CONF_PATH)?;

        let mut header_lines = Vec::new();
        let mut networks = Vec::new();

        let mut in_network = false;
        let mut cur_ssid = String::new();
        let mut cur_key_mgmt = "WPA-PSK".to_string();
        let mut cur_psk: Option<String> = None;
        let mut cur_disabled = false;
        let mut cur_scan_ssid = false;
        let mut cur_extra = Vec::new();

        for raw_line in content.lines() {
            if raw_line.len() > MAX_CONFIG_LINE_BYTES {
                return Err(format!(
                    "Wi-Fi config line exceeds {} bytes",
                    MAX_CONFIG_LINE_BYTES
                ));
            }
            let line = raw_line.trim();
            if !in_network {
                if line.starts_with("network={") || line.starts_with("network = {") {
                    in_network = true;
                    cur_ssid.clear();
                    cur_key_mgmt = "WPA-PSK".to_string();
                    cur_psk = None;
                    cur_disabled = false;
                    cur_scan_ssid = false;
                    cur_extra.clear();
                } else if !line.is_empty() {
                    header_lines.push(raw_line.to_string());
                }
            } else if line == "}" {
                in_network = false;
                if !cur_ssid.is_empty() {
                    if networks.len() >= MAX_NETWORKS {
                        return Err(format!("Wi-Fi config exceeds {} networks", MAX_NETWORKS));
                    }
                    let id = networks.len();
                    networks.push(StoredNetwork {
                        id,
                        ssid: cur_ssid.clone(),
                        key_mgmt: cur_key_mgmt.clone(),
                        psk: cur_psk.clone(),
                        is_primary: !cur_disabled,
                        scan_ssid: cur_scan_ssid,
                        extra_lines: cur_extra.clone(),
                    });
                }
            } else if let Some((k, v)) = line.split_once('=') {
                let key = k.trim();
                let val = v.trim();
                match key {
                    "ssid" => cur_ssid = trim_quotes(val),
                    "key_mgmt" => cur_key_mgmt = val.to_string(),
                    "psk" => cur_psk = Some(val.to_string()), // Сохраняем как есть (в кавычках или raw hex)
                    "disabled" => cur_disabled = val == "1",
                    "scan_ssid" => cur_scan_ssid = val == "1",
                    "auth_alg" => {} // Добавляется автоматически при сериализации
                    _ => cur_extra.push(format!("        {}={}", key, val)),
                }
            }
        }

        if header_lines.is_empty() {
            header_lines = DEFAULT_HEADER.lines().map(String::from).collect();
        }

        // Гарантируем, что не более одной сети помечено как основная
        let mut found_primary = false;
        for net in &mut networks {
            if net.is_primary {
                if !found_primary {
                    found_primary = true;
                } else {
                    net.is_primary = false;
                }
            }
        }

        Ok(Self {
            header_lines,
            networks,
        })
    }

    /// Атомарно сохраняет конфигурацию через `.tmp` -> `fsync()` -> `rename()`.
    pub fn save_atomic(&self) -> Result<(), String> {
        let mut out = String::new();
        for h in &self.header_lines {
            out.push_str(h);
            out.push('\n');
        }
        out.push('\n');

        if self.networks.len() > MAX_NETWORKS {
            return Err(format!("Wi-Fi config exceeds {} networks", MAX_NETWORKS));
        }
        if out.len() as u64 > MAX_CONFIG_BYTES {
            return Err(format!("Wi-Fi config exceeds {} bytes", MAX_CONFIG_BYTES));
        }

        for net in &self.networks {
            out.push_str("network={\n");
            out.push_str(&format!("        ssid=\"{}\"\n", escape_quotes(&net.ssid)));
            out.push_str(&format!("        key_mgmt={}\n", net.key_mgmt));
            out.push_str("        auth_alg=OPEN\n");

            if let Some(ref psk_val) = net.psk {
                out.push_str(&format!("        psk={}\n", psk_val));
            }
            if net.scan_ssid {
                out.push_str("        scan_ssid=1\n");
            }
            for extra in &net.extra_lines {
                if extra.len() > MAX_CONFIG_LINE_BYTES {
                    return Err(format!(
                        "Wi-Fi config line exceeds {} bytes",
                        MAX_CONFIG_LINE_BYTES
                    ));
                }
                out.push_str(extra);
                out.push('\n');
            }
            if !net.is_primary {
                out.push_str("        disabled=1\n");
            }
            out.push_str("}\n\n");
        }
        if out.len() as u64 > MAX_CONFIG_BYTES {
            return Err(format!("Wi-Fi config exceeds {} bytes", MAX_CONFIG_BYTES));
        }

        let mut tmp_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(WPA_CONF_TMP_PATH)
            .map_err(|e| format!("Failed to open tmp config: {}", e))?;

        tmp_file
            .write_all(out.as_bytes())
            .map_err(|e| format!("Failed to write tmp config: {}", e))?;

        tmp_file
            .sync_all()
            .map_err(|e| format!("fsync failed on tmp config: {}", e))?;

        drop(tmp_file);

        // Сначала фиксируем новую подтверждённую версию в .bak. Если питание
        // пропадёт между этими операциями, следующий load() восстановит
        // основной файл из уже новой резервной копии.
        fs::copy(WPA_CONF_TMP_PATH, WPA_CONF_BAK_PATH)
            .map_err(|e| format!("Failed to update config backup: {}", e))?;
        let _ = fs::set_permissions(WPA_CONF_BAK_PATH, fs::Permissions::from_mode(0o600));

        fs::rename(WPA_CONF_TMP_PATH, WPA_CONF_PATH)
            .map_err(|e| format!("Atomic rename failed: {}", e))?;

        let _ = fs::set_permissions(WPA_CONF_PATH, fs::Permissions::from_mode(0o600));
        Ok(())
    }

    /// Возвращает список сохранённых сетей без паролей для API.
    pub fn to_saved_dtos(&self) -> Vec<SavedNetworkDto> {
        self.networks
            .iter()
            .map(|n| SavedNetworkDto {
                id: n.id,
                ssid: n.ssid.clone(),
                key_mgmt: n.key_mgmt.clone(),
                is_primary: n.is_primary,
            })
            .collect()
    }

    /// Возвращает текущую основную сеть, если она задана.
    pub fn get_primary(&self) -> Option<&StoredNetwork> {
        self.networks.iter().find(|n| n.is_primary)
    }

    /// Ищет сохранённую сеть по ID.
    pub fn find_by_id(&self, id: usize) -> Option<&StoredNetwork> {
        self.networks.iter().find(|n| n.id == id)
    }

    /// Ищет сохранённую сеть по SSID.
    pub fn find_by_ssid(&self, ssid: &str) -> Option<&StoredNetwork> {
        self.networks.iter().find(|n| n.ssid == ssid)
    }

    /// Добавляет новую или обновляет существующую сеть и назначает её единственной основной.
    pub fn upsert_network(
        &mut self,
        ssid: &str,
        password: Option<&str>,
        is_hidden: bool,
        make_primary: bool,
    ) {
        if make_primary {
            for net in &mut self.networks {
                net.is_primary = false;
            }
        }

        let formatted_psk = password.and_then(|p| {
            if p.is_empty() {
                None
            } else {
                Some(format!("\"{}\"", escape_quotes(p)))
            }
        });

        if let Some(existing) = self.networks.iter_mut().find(|n| n.ssid == ssid) {
            if formatted_psk.is_some() {
                existing.psk = formatted_psk;
                existing.key_mgmt = "WPA-PSK".to_string();
            } else if password == Some("") && existing.psk.is_none() {
                existing.key_mgmt = "NONE".to_string();
            }
            if is_hidden {
                existing.scan_ssid = true;
            }
            if make_primary {
                existing.is_primary = true;
            }
        } else {
            let key_mgmt = if formatted_psk.is_some() {
                "WPA-PSK".to_string()
            } else {
                "NONE".to_string()
            };
            let id = self.networks.len();
            self.networks.push(StoredNetwork {
                id,
                ssid: ssid.to_string(),
                key_mgmt,
                psk: formatted_psk,
                is_primary: make_primary,
                scan_ssid: is_hidden,
                extra_lines: Vec::new(),
            });
        }

        self.reindex();
    }

    /// Делает существующую сохранённую сеть основной по её ID.
    pub fn set_primary_by_id(&mut self, target_id: usize) -> Result<String, String> {
        if !self.networks.iter().any(|n| n.id == target_id) {
            return Err(format!("Network with id {} not found", target_id));
        }
        let mut target_ssid = String::new();
        for net in &mut self.networks {
            if net.id == target_id {
                net.is_primary = true;
                target_ssid = net.ssid.clone();
            } else {
                net.is_primary = false;
            }
        }
        Ok(target_ssid)
    }

    /// Удаляет сохранённую сеть по ID.
    pub fn remove_by_id(&mut self, target_id: usize) -> Result<(), String> {
        let prev_len = self.networks.len();
        self.networks.retain(|n| n.id != target_id);
        if self.networks.len() == prev_len {
            return Err(format!("Network with id {} not found", target_id));
        }
        self.reindex();
        Ok(())
    }

    fn reindex(&mut self) {
        for (idx, net) in self.networks.iter_mut().enumerate() {
            net.id = idx;
        }
    }
}

fn trim_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        trimmed.to_string()
    }
}

fn escape_quotes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Возвращает чистый пароль сохранённой сети (без обрамляющих кавычек) для передачи в wpa_ctrl.
pub fn extract_clean_psk(stored: &StoredNetwork) -> Option<String> {
    stored.psk.as_ref().map(|raw| trim_quotes(raw))
}

#[cfg(test)]
mod tests {
    use super::{StoredNetwork, WpaConfigFile, MAX_CONFIG_LINE_BYTES, MAX_NETWORKS};

    fn config_with_networks() -> WpaConfigFile {
        WpaConfigFile {
            header_lines: vec![
                "ctrl_interface=/var/run/wpa_supplicant".to_string(),
                "update_config=1".to_string(),
            ],
            networks: vec![
                StoredNetwork {
                    id: 0,
                    ssid: "First".to_string(),
                    key_mgmt: "WPA-PSK".to_string(),
                    psk: Some("\"firstpass\"".to_string()),
                    is_primary: true,
                    scan_ssid: false,
                    extra_lines: Vec::new(),
                },
                StoredNetwork {
                    id: 1,
                    ssid: "Second".to_string(),
                    key_mgmt: "WPA-PSK".to_string(),
                    psk: Some("\"secondpass\"".to_string()),
                    is_primary: false,
                    scan_ssid: false,
                    extra_lines: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn removing_network_reindexes_remaining_profiles() {
        let mut config = config_with_networks();

        config.remove_by_id(0).expect("network should exist");

        assert_eq!(config.networks.len(), 1);
        assert_eq!(config.networks[0].id, 0);
        assert_eq!(config.networks[0].ssid, "Second");
        assert!(!config.networks[0].is_primary);
    }

    #[test]
    fn save_rejects_too_many_networks_before_touching_disk() {
        let mut config = config_with_networks();
        config
            .networks
            .extend((2..=MAX_NETWORKS).map(|id| StoredNetwork {
                id,
                ssid: format!("network-{id}"),
                key_mgmt: "WPA-PSK".to_string(),
                psk: Some("\"password\"".to_string()),
                is_primary: false,
                scan_ssid: false,
                extra_lines: Vec::new(),
            }));
        assert_eq!(config.networks.len(), MAX_NETWORKS + 1);
        let error = config
            .save_atomic()
            .expect_err("oversized profile list must be rejected");
        assert!(error.contains("networks"));
    }

    #[test]
    fn save_rejects_oversized_line() {
        let mut config = config_with_networks();
        config.networks[0].extra_lines.push(format!(
            "        vendor_field={}",
            "x".repeat(MAX_CONFIG_LINE_BYTES)
        ));
        let error = config
            .save_atomic()
            .expect_err("oversized serialized config must be rejected");
        assert!(error.contains("line"));
    }

    #[test]
    fn upserting_primary_network_demotes_previous_primary() {
        let mut config = config_with_networks();

        config.upsert_network("Third", Some("thirdpass"), false, true);

        assert_eq!(
            config.get_primary().map(|network| network.ssid.as_str()),
            Some("Third")
        );
        assert_eq!(
            config
                .networks
                .iter()
                .filter(|network| network.is_primary)
                .count(),
            1
        );
    }
}
