<p align="center">
  <img src="assets/brand/banner.png" alt="Kryo Shard — Hit a key. Keep the moment." width="100%">
</p>

<p align="center">
  <b>Free, open-source game clipping for Windows.</b><br>
  Hit a key, keep the moment: instant clips, a built-in editor and one-click Discord sharing.<br>
  100% local. No account, no cloud, no watermark.
</p>

<p align="center">
  <img alt="Status: early development" src="https://img.shields.io/badge/status-early%20development-3EC6FF?style=flat-square&labelColor=0C0E12">
  <img alt="Platform: Windows" src="https://img.shields.io/badge/platform-Windows-3EC6FF?style=flat-square&labelColor=0C0E12">
  <img alt="License: GPL-3.0" src="https://img.shields.io/badge/license-GPL--3.0-3EC6FF?style=flat-square&labelColor=0C0E12">
</p>

---

> **Kryo Shard is in early development.** There is no download yet: the first public beta is on its way.

<p align="center">
  <img src="assets/preview.png" alt="The Glacier, where your shards live" width="90%">
</p>

## Why Kryo Shard

- **Always ready.** The last seconds of gameplay are kept in memory. Press a key and they are saved instantly, no re-encoding.
- **Light.** Encoding runs on your graphics card (NVIDIA, AMD or Intel). The impact on your FPS should be invisible.
- **Yours.** Everything stays on your PC. Clips are plain MP4 files. Nothing is uploaded unless you decide to.
- **Anti-cheat safe.** Kryo Shard captures the screen like OBS or Discord do. It never injects into a game or reads its memory.
- **Simple.** Works out of the box. Advanced options are there, tucked away.

A clip is called a **shard**. Your library is the **Glacier**.

## Roadmap to the first beta

- [x] Brand, app shell and Glacier preview (FR / EN)
- [x] App foundation: settings, global hotkey, tray icon, launch at Windows startup, single instance
- [x] Glacier: shards sorted by game, favorites, search, rename, built-in player, copy a clip for Discord, Recycle Bin
- [x] Always-on replay buffer (capture engine), capture sound and in-game notification
- [x] Separate audio tracks: game and microphone, plus a mix that every player picks by default
- [x] Editor: trim bar, mute or lower each audio track
- [x] Discord export: fit a clip under 10 MB, 50 MB or 500 MB, copy and paste it straight into Discord
- [x] Installer and signed automatic updates
- [ ] Windows code signing (no SmartScreen warning)
- [ ] Separate Discord voice track, mouse cursor in clips, choice of the captured screen

**Later:** auto-clips (Rocket League first), vertical 9:16 format, slow motion, text and local subtitles, importing clips from ShadowPlay / OBS / Medal, Discord Rich Presence, community translations and detectors.

## Building from source

Requirements: Windows 10 or 11, [Node.js](https://nodejs.org) 20+, [Rust](https://rustup.rs) and the Visual Studio C++ build tools ([Tauri prerequisites](https://tauri.app/start/prerequisites/)).

```bash
npm install
npm run tauri dev     # run the app with hot reload
npm run tauri build   # build the installer
```

`tauri build` also signs the in-app update files, so it needs the update signing key: set `TAURI_SIGNING_PRIVATE_KEY` to the key (or its path) first. Forks generate their own pair with `npx tauri signer generate` and put the public key in `src-tauri/tauri.conf.json`.

Releases are built by GitHub Actions: push a tag such as `v0.2.0` (matching the version in `tauri.conf.json`) and a draft release is created with the installer and the signed update manifest.

### How capture works

The screen is read with DXGI Desktop Duplication, the same Windows API that OBS and Discord use, so nothing is injected into games. Frames are encoded on the graphics card (NVENC, AMF or Quick Sync through Media Foundation, with a software fallback) and the last seconds are kept in memory. Game sound comes from WASAPI loopback and the microphone from its own device, on separate tracks. Pressing the hotkey writes an MP4 straight from memory, without re-encoding.

Built with Rust, [Tauri](https://tauri.app) and [Svelte](https://svelte.dev).

## License

Kryo Shard is free software, released under the [GNU GPL v3.0](LICENSE). You can use it, study it, change it and share it. If you distribute a modified version, it must stay free and open source under the same license, with its source code available.

The **Kryo Shard** and **Kryo Studio** names and logos are not covered by this license. Forks must use their own name and logo: see [TRADEMARKS.md](TRADEMARKS.md) and [NOTICE](NOTICE).

Copyright © 2026 Kryo Studio.
