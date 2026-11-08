# Сборка

## Rust-бинарник

Целевой бинарник:

```text
target/armv7-unknown-linux-musleabihf/release/alice-wifi-manager
```

Предпочтительная команда:

```powershell
cargo zigbuild --release --target armv7-unknown-linux-musleabihf
```

Минимальная проверка исходников:

```powershell
cargo fmt -- --check
cargo check --target armv7-unknown-linux-musleabihf
```

`cargo check` не заменяет линковку release-бинарника.

## wpa_supplicant

Вторая часть продукта — проверенная отдельная сборка `wpa_supplicant 2.9` с
`CONFIG_AP=y` и `CONFIG_DRIVER_NL80211=y`. Нельзя заменять её штатной
версией прошивки.

Проверенный бинарник находится в `artifacts/armv7/wpa_supplicant`; release
pipeline сверяет его SHA256 перед упаковкой. Это подтверждает целостность
распространяемого файла, но не означает автоматически воспроизводимость его
сборки.

## Встроенный веб-интерфейс

`web/index.html` встраивается через `include_str!`, поэтому после любого
изменения HTML необходима новая сборка `alice-wifi-manager`.
