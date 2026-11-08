# Диагностика

Основные файлы:

```text
/bkupgrade/data/ap_debug.log
/bkupgrade/data/ap_wpa_supplicant.log
/bkupgrade/data/sta_wpa_supplicant.log
```

Состояние:

```sh
/usr/bin/alice-wifi-manager status
wpa_cli -i wlan0 status
ps | grep -E 'alice-wifi-manager|wpa_supplicant|dhcpcd|platformd'
```

Если сеть отображается как доступная после отключения роутера, учитывайте,
что live scan в AP-режиме на `sprdwl_ng` ненадёжен. Список предварительного
сканирования нельзя считать доказательством текущей доступности.

Если профили возвращаются после AP→STA, проверяйте основной конфиг и `.bak`:
они должны содержать одинаковый последний подтверждённый список сетей.

