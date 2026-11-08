# Alice WiFi Manager

Автономный менеджер Wi-Fi для Яндекс Станции Mini 2. Проект управляет
переключением STA/AP, сохранёнными профилями, DHCP/DNS, captive portal и
мобильной страницей настройки.

## Статус

Основные STA/AP-переходы и сохранение профилей проверены на рабочем проекте.
Публикация стабильного релиза требует отдельного аппаратного прогона на Mini 2:
host-проверки не заменяют проверку колонки.

В репозитории хранится проверенный модифицированный `wpa_supplicant` 2.9 для
ARMv7. Его SHA256:

```text
3e7a5dd461d24fa72e8cc9ab53f1a2274f2df61a14a5ffe8cdb8b0f8d151f3bc
```

YAM в этот проект не входит.

## Возможности

- один экземпляр менеджера и один экземпляр `wpa_supplicant`;
- STA -> AP после 20 секунд отсутствия подтверждённого IPv4;
- AP `Alice-Mini2-Setup`, канал 6, `192.168.4.1/24`;
- встроенные DHCP/DNS и HTTP captive portal;
- добавление, выбор и удаление сохранённых сетей;
- сохранение профилей через основной конфиг и подтверждённый `.bak`;
- периодическая проверка сохранённых сетей в AP;
- транзакционный rollback при ошибке DHCP, DNS или подключения;
- BusyBox init-скрипт без зависимости от systemd.

Постоянные параметры AP сейчас заданы проектом:

```text
SSID: Alice-Mini2-Setup
Password: alice1234
IP: 192.168.4.1/24
```

## Структура

```text
src/                 Rust state machine, AP, HTTP, config and WPA control
web/index.html       Embedded mobile UI
scripts/             BusyBox init and SSH installers
docs/                Build, installation, recovery and architecture docs
artifacts/armv7/     Verified wpa_supplicant artifact
.github/workflows/   CI, ARM build and guarded release pipeline
```

`web/index.html` встраивается в бинарник через `include_str!`. После изменения
HTML обязательна новая сборка `alice-wifi-manager`.

## Сборка

Проверки исходников:

```powershell
cargo fmt -- --check
cargo check --target armv7-unknown-linux-musleabihf
cargo test --target x86_64-unknown-linux-gnu
```

Release-сборка:

```powershell
cargo zigbuild --release --target armv7-unknown-linux-musleabihf
```

Результат:

```text
target/armv7-unknown-linux-musleabihf/release/alice-wifi-manager
```

Бинарник проверен для целевой Mini 2. Воспроизводимость его сборки — отдельная
задача: для неё необходимо сохранить исходники, `.config`, компилятор, sysroot
и хэши входных архивов.

## Установка и обновление

Для устройства требуется рабочий SSH-доступ по сети или другой сервисный
канал. Обычный USB-доступ к Mini 2 не предполагается.

Из директории с тремя файлами пакета:

```powershell
.\scripts\install.ps1 -HostName 192.168.1.50
```

Linux/macOS:

```sh
./scripts/install.sh --host 192.168.1.50
```

Скрипты создают recovery-копии, сохраняют Wi-Fi-конфигурацию, заменяют только
наши файлы, перезапускают менеджер и проверяют единственный экземпляр.
`update.ps1` и `update.sh` используют тот же защищённый путь.

Удаление менеджера без удаления Wi-Fi профилей:

```powershell
.\scripts\uninstall.ps1 -HostName 192.168.1.50
```

## CLI на устройстве

```text
alice-wifi-manager daemon
alice-wifi-manager start-ap
alice-wifi-manager stop-ap
alice-wifi-manager status
alice-wifi-manager diag-ap
```

## Диагностика

```text
/bkupgrade/data/ap_debug.log
/bkupgrade/data/ap_wpa_supplicant.log
/bkupgrade/data/sta_wpa_supplicant.log
/bkupgrade/data/wpa_supplicant.conf
/bkupgrade/data/wpa_supplicant.conf.bak
```

Полный чек-лист находится в `docs/TROUBLESHOOTING.md` и
`docs/RECOVERY.md`.

## Ограничения

- live scan в AP-режиме на `sprdwl_ng` ненадёжен; предварительный scan не
  является доказательством текущей доступности сети;
- произвольная offline-озвучка голосом Алисы не гарантируется: штатный IPC
  использует облачный TTS;
- аппаратный тест на Mini 2 не выполняется GitHub Actions;
- release workflow останавливается, если проверенный `wpa_supplicant`
  отсутствует или SHA256 не совпадает.

## Безопасность

Не добавляйте в репозиторий реальные `wpa_supplicant.conf`, пароли, логи или
ключи. См. `SECURITY.md`.
