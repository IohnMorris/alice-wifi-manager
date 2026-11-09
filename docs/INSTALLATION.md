# Установка

Проект пока протестирован исключительно на Яндекс Станции Mini 2. Поддержка
других устройств не заявляется. Перед началом нужен SSH-доступ к колонке.

Самый простой вариант на Linux/macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/IohnMorris/alice-wifi-manager/main/scripts/bootstrap.sh | sh -s -- --host 192.168.1.50
```

Bootstrap скачивает последний релиз, проверяет SHA256 и запускает установку.
На колонке сохраняется recovery-копия текущих бинарников и конфигураций.

Установка выполняется только при наличии рабочего канала к устройству:
SSH по сети, ADB по сети или сервисный доступ. Обычный USB-доступ к Mini 2
не предполагается.

Перед заменой файлов необходимо сохранить:

```text
/bkupgrade/data/wpa_supplicant.conf
/bkupgrade/data/wpa_supplicant.conf.bak
/etc/init.d/S40wifi
/usr/sbin/wpa_supplicant
```

Установка не должна удалять сохранённые Wi-Fi профили или менять штатный
`S40wifi` без отдельной проверки.

После установки проверить:

```sh
/usr/bin/alice-wifi-manager status
ps | grep alice-wifi-manager
test -S /var/run/alice-wifi-manager.sock
```

Аппаратная проверка обязательна: STA, AP, DHCP, DNS, HTTP и rollback должны
быть проверены на реальной Mini 2 отдельно от host-сборки.
