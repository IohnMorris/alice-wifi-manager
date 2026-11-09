# Восстановление

Не выполняйте factory reset как первую реакцию. Сначала сохраните логи и
конфигурацию через доступный канал:

```sh
cp /bkupgrade/data/wpa_supplicant.conf /bkupgrade/data/wpa_supplicant.conf.recovery
cp /bkupgrade/data/wpa_supplicant.conf.bak /bkupgrade/data/wpa_supplicant.conf.bak.recovery
cp /bkupgrade/data/ap_debug.log /bkupgrade/data/ap_debug.log.recovery 2>/dev/null || true
cp /bkupgrade/data/sta_wpa_supplicant.log /bkupgrade/data/sta_wpa_supplicant.log.recovery 2>/dev/null || true
```

Для удаления установленного менеджера из SSH-сессии:

```sh
/usr/bin/alice-wifi-manager uninstall --yes
```

Перед удалением команда создаёт каталог
`/bkupgrade/data/recovery-uninstall-*`. Wi-Fi-профили и `wpa_supplicant`
сохраняются; удаляются только менеджер, его init-скрипт, runtime-файлы и
временный AP-конфиг.

Для возврата STA:

```sh
/etc/init.d/S99z-wifi-manager stop
kill -CONT "$(pidof maind)" 2>/dev/null || true
kill -CONT "$(pidof dhcpcd)" 2>/dev/null || true
/etc/init.d/S40wifi restart
```

Команды выполняйте только после проверки наличия соответствующего init-скрипта
и процесса. Не удаляйте `/bkupgrade/data/wpa_supplicant.conf`.
