# Установка

Проект протестирован на Яндекс Станции Mini 2 (Buildroot Linux 4.9.68, ARMv7,
`wlan0`, `sprdwl_ng`). Перед началом нужен рабочий SSH-доступ к колонке и
резервный сервисный канал, если он доступен.

Самый простой вариант на Linux/macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/IohnMorris/alice-wifi-manager/main/scripts/bootstrap.sh | sh -s -- --host 192.168.1.50
```

В Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/IohnMorris/alice-wifi-manager/main/scripts/bootstrap.ps1 -OutFile "$env:TEMP\alice-install.ps1"; powershell -ExecutionPolicy Bypass -File "$env:TEMP\alice-install.ps1" -HostName 192.168.1.50
```

Bootstrap скачивает ARMv7-релиз и проверяет SHA256 архива. Установщик затем:

1. загружает manager, проверенный модифицированный `wpa_supplicant` и init-
   скрипт во временный каталог;
2. создаёт lock и recovery-каталог, сохраняя старые файлы и Wi-Fi-конфиги;
3. проверяет запись на целевые файловые системы и останавливает сервис;
4. атомарно заменяет подготовленные файлы на одной файловой системе;
5. запускает сервис и проверяет PID, control socket, `status` и процессы WPA;
6. при любой ошибке восстанавливает предыдущие файлы и оставляет recovery.

Старые recovery-каталоги ограниченно очищаются только после успешной установки;
каталог неудачной транзакции сохраняется. Сохранённые Wi-Fi-профили не
удаляются и не заменяются установщиком.

Для установки локального пакета:

```sh
./scripts/install.sh --host 192.168.1.50 --package ./package
```

```powershell
.\scripts\install.ps1 -HostName 192.168.1.50 -PackageDir .\package
```

После установки проверьте:

```sh
/usr/bin/alice-wifi-manager status
ps | grep -E 'alice-wifi-manager|wpa_supplicant'
test -S /var/run/alice-wifi-manager.sock
```

Аппаратная проверка обязательна отдельно от host-сборки: STA, AP, DHCP, DNS,
HTTP, перезагрузка и реальный rollback на Mini 2.
