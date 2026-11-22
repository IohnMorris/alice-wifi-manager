# Восстановление

Не выполняйте factory reset как первую реакцию. Сначала сохраните доступный
диагностический файл и конфигурацию через SSH:

```sh
cp /bkupgrade/data/wpa_supplicant.conf /bkupgrade/data/wpa_supplicant.conf.recovery
cp /bkupgrade/data/wpa_supplicant.conf.bak /bkupgrade/data/wpa_supplicant.conf.bak.recovery
cp /bkupgrade/data/ap_debug.log /bkupgrade/data/ap_debug.log.recovery 2>/dev/null || true
```

При неудачном обновлении установщик автоматически возвращает предыдущие версии
manager, `wpa_supplicant` и init-скрипта. Не удаляйте каталог
`/bkupgrade/data/recovery-*`: он содержит manifest-копии для ручного возврата.
Если автоматический rollback не смог запустить старую версию, подключитесь по
SSH и выполните:

```sh
RECOVERY=/bkupgrade/data/recovery-YYYYMMDD-HHMMSS-PID
cp "$RECOVERY/alice-wifi-manager" /usr/bin/alice-wifi-manager
cp "$RECOVERY/wpa_supplicant" /usr/sbin/wpa_supplicant
cp "$RECOVERY/S99z-wifi-manager" /etc/init.d/S99z-wifi-manager
chmod 0755 /usr/bin/alice-wifi-manager /usr/sbin/wpa_supplicant /etc/init.d/S99z-wifi-manager
/etc/init.d/S99z-wifi-manager start
/usr/bin/alice-wifi-manager status
```

Для удаления установленного менеджера из SSH-сессии:

```sh
/usr/bin/alice-wifi-manager uninstall --yes
```

Команда сначала сохраняет recovery-копию, останавливает сервис и удаляет только
manager, его init-скрипт, runtime-файлы и временный AP-конфиг. `wpa_supplicant`,
основной Wi-Fi-конфиг и сохранённые сети не удаляются.

Для возврата STA при аварии:

```sh
/etc/init.d/S99z-wifi-manager stop
kill -CONT "$(pidof maind)" 2>/dev/null || true
kill -CONT "$(pidof dhcpcd)" 2>/dev/null || true
/etc/init.d/S40wifi restart
```

Команды выполняйте только после проверки наличия соответствующего init-скрипта
и процесса. Не удаляйте `/bkupgrade/data/wpa_supplicant.conf`.
