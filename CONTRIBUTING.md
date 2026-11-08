# Contributing

## Scope

Changes must preserve the single-`wpa_supplicant` design, the STA/AP rollback
path, and the Buildroot/BusyBox constraints of Yandex Station Mini 2.

Do not add a second `maind`, a second Wi-Fi manager instance, or a dependency
on systemd, GNU-only utilities, or a live internet connection on the device.

## Validation

Before submitting a change, run:

```powershell
cargo fmt -- --check
cargo check --target armv7-unknown-linux-musleabihf
cargo test --target armv7-unknown-linux-musleabihf
```

Hardware-only behavior must be reported separately from host or cross-target
checks. Do not describe a simulated test as a Mini 2 hardware test.

