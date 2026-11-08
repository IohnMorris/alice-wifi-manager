# Changelog

## Unreleased

- Fixed saved-network persistence across AP/STA transitions.
- Prevented AP startup and connection tests from overwriting the committed
  configuration backup with stale `wpa_supplicant` runtime data.
- Added release and recovery documentation scaffolding.

No stable release is published until the ARMv7 binary, the modified
`wpa_supplicant`, installation, update, and rollback are verified on a real
Yandex Station Mini 2.

