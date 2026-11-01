# 🛋️ CouchPlay

Stream your game to friends in a Discord voice channel and pass the controller around.

**Status: step 1 (feasibility spike) and the native Windows host are built.** The spike answers the riskiest question:

> Can a **Discord Activity** receive low-latency **WebRTC video** and read a **controller** (Gamepad API), and can we hand control between friends cleanly?

The host can run in a browser tab (`host.html`, `getDisplayMedia`) or as the native Rust app in `host/` (screen capture, NVENC H.264, virtual Xbox 360 pad). Both speak the same signaling protocol, so the viewer doesn't care which one is hosting.

## How it works

```
Host tab (host.html)                     Viewer (index.html: Discord Activity or browser)
  getDisplayMedia ─► WebRTC video ─────► <video>  (jitter buffer target 0)
  virtual pad  ◄──── data channel "input" ◄── Gamepad API / keyboard, 125 Hz, 22-byte packets
  ping/pong   ◄───► data channel "ctrl"
        ▲                                          ▲
        └──────── server/ (signaling + control token over WebSocket) ┘
```

- **Control token** (`server/rooms.js`): one viewer at a time holds the controller. Viewers can `take` or `release` it, and the host can `give` it to anyone or take it back. The host drops input from anyone who isn't the holder.
- **Handoff safety**: when control changes hands the host resets the pad to neutral, so no button stays held.
- **Input packets** (`client/src/input.js`): every packet carries the full pad state and goes over an unordered, no-retransmit channel, so a lost packet costs nothing and a late one gets dropped.

## Run it locally (no Discord)

```bash
npm install
npm run build
npm start                       # http://localhost:3001
```

1. Open http://localhost:3001/host.html, choose **Start party** and share a window or screen.
2. Open the link it shows (`/?room=CODE`) in another tab or on another PC on your LAN, enter a name and join.
3. Choose **Take control** and press buttons on a controller, or use the keyboard fallback. The host's "Virtual controller" panel lights up.

For live-reload development run `npm run dev:server` (API on :3001) and `npm run dev` (Vite on :5173) in two terminals, then open http://localhost:5173/host.html.

Tests (with `npm start` running in another terminal):
- `npm run smoke`: signaling and control-token rules.
- `npm run e2e`: real headless Chrome with a fake screen share. Checks that video arrives, take control works, input reaches the host and passing control back resets the pad. Set `CHROME_PATH` if Chrome isn't auto-detected.

## Run the native host (Windows)

```bash
npm run build && npm start                  # server on :3001, in one terminal
cd host && cargo run --release              # native host, in another
```

It prints a room code and link. Friends join exactly as with the browser host. Options (or `COUCHPLAY_*` env vars): `--server ws://…`, `--fps 60`, `--kbps 12000`, `--height 1080` (0 = native), `--monitor N`, `--window TEXT`, `--list-sources`, `--audio-kbps 128`, `--no-audio`, `--audio-exclude Discord.exe,…`, `--ice stun:…,turn:…`. It captures the primary monitor (or `--monitor N`, or one window with `--window "part of the title"`; run `--list-sources` to see what's available) plus system audio, and encodes only while someone is watching.

**Window capture:** the stream keeps the aspect ratio (black bars) when the window is resized, and if the window closes or the game isn't started yet the host waits and picks it up again when it appears.

**Audio and Discord echo:** by default the stream carries everything *except* Discord's process tree (Windows process loopback), so friends in the voice channel don't hear themselves played back. If Discord isn't running it captures all system audio and switches over within ~5 s of Discord starting. Pass `--audio-exclude ""` to capture everything.

For game input, install the [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) driver. Without it the host still streams, and logs that game input is disabled.

Test: `cd host && cargo build`, then with the server running `EXPECT_VIDEO=1 npm run e2e:native` (real headless Chrome decodes the native host's video, takes control and sends input). `cargo test` covers the packet decoder, the XInput mapping and the NAL scanner. Set `E2E_SCREENSHOT=file.png` to save a decoded frame, and `EXPECT_AUDIO=1` to play a short Windows sound and require Chrome to decode it from the stream, and `E2E_WINDOW=1` to stream a self-resizing test window (`scripts/e2e-window.ps1`) instead of the monitor.

## Run it inside Discord

1. Create an app at https://discord.com/developers/applications.
2. **OAuth2**: copy the Client ID and Client Secret into `.env` (see `.env.example`). Add `https://127.0.0.1` as a redirect URI. Discord requires one, but it isn't used.
3. Start a public HTTPS tunnel to the server:
   ```bash
   npm run build && npm start
   cloudflared tunnel --url http://localhost:3001     # prints https://<random>.trycloudflare.com
   ```
   Build **after** creating `.env`. The client ID is compiled into the page, and without it the Discord code is left out.
4. **Activities → Settings**: enable Activities. **Activities → URL Mappings**: map `/` to the tunnel host (`<random>.trycloudflare.com`, without `https://`).
5. **Installation**: install the app to your server.
6. Host: open `http://localhost:3001/host.html` in Chrome, **not** inside Discord, because screen capture needs a normal tab. Choose **Start party**.
7. Friends: join the voice channel → 🚀 Activities → your app → enter the room code.

## What to look for (the point of the spike)

The viewer's **Diagnostics** panel reports each risk:

| Row | Good | If it fails |
|---|---|---|
| Discord | `signed in as …` | OAuth / URL-mapping setup |
| Gamepad API | `available` | Discord's iframe blocks it. The keyboard fallback still works, but we'd need a workaround, e.g. opening the player in a browser tab. |
| Controller | your pad's name | Press a button, since browsers hide pads until you do |
| WebRTC | `connected` | Discord's sandbox blocks peer-to-peer WebRTC → fall back to a browser link, or route media through a server |
| Route | `host`/`srflx` | `relay` means it went through TURN (more latency). No route at all means NAT trouble, so add a TURN server in `.env`. |
| Network RTT / Ping | as low as your connection allows | — |
| Jitter buffer | under ~30 ms | Higher values mean the browser is buffering for smoothness |

## Status & next steps

_Last updated: 2026-11-01. Read this first when picking the project up on a new machine._

### Done
- Step 1 spike, working and tested in a browser (`npm run smoke`, `npm run e2e` both pass).
- **Native Windows host (`host/`, Rust).** Windows Graphics Capture → GPU colour conversion (D3D11 video processor, BGRA→NV12) → Media Foundation hardware H.264 (NVENC on this PC's RTX 3070) → `webrtc-rs`, same signaling and 22-byte input format. Verified with `npm run e2e:native`: Chrome decodes 1920x1080 at a steady 60 fps (~5 Mbit/s on a desktop), the control token and input gating work.
- **Audio**: WASAPI loopback → 48 kHz stereo → Opus (20 ms, 128 kbps) as a second track in the same stream. Excludes Discord's process tree so there's no echo; falls back to whole-system loopback (compensating for the Windows volume level). Verified: Chrome decodes it, an excluded app is silent, a non-excluded app passes at full level.
- **Virtual gamepad** code (ViGEm Xbox 360, `host/src/pad.rs`): button/stick mapping is unit-tested, the neutral reset on handoff is wired in.

### Not done / unverified
- **Never run inside Discord yet.** Deliberately last. Needs a Discord developer app (see "Run it inside Discord"). The two unknowns:
  1. Does WebRTC video connect inside the Activity iframe? (Diagnostics → WebRTC / Route)
  2. Does the Gamepad API work there? (Diagnostics → Gamepad API / Controller)
- **Game input is untested against a real game**: ViGEmBus isn't installed on the dev PC, so only the mapping is tested.
- Window capture is tested with a WinForms test window, not a real game. Exclusive-fullscreen games usually can't be captured by Windows Graphics Capture; borderless/windowed works. The stream resolution is fixed at start, so a window resized later is letterboxed rather than re-encoded at its new size.
- Audio: if the Windows output is *muted*, whole-system loopback is silent too (the Discord-excluding mode isn't affected). Audio and video aren't explicitly lip-synced beyond sharing a stream; not measured yet.
- No TURN server, so friends on strict NATs may fail to connect.
- Encoder is H.264 Constrained Baseline at CBR. Quality/latency tuning (profile, rate control, bitrate adaptation, PLI handling) hasn't been done.

### Decisions made
- **Main dev platform: Windows.** That's where users' games and the installer are, DXGI/WGC capture needs no permission prompts, and ViGEm gives games a real Xbox pad. Linux (PipeWire + uinput) may come later.
- **Friends need zero installs**: Discord Activity, or a browser link if the Activity sandbox blocks something. That's why it uses WebRTC instead of Moonlight.
- **Discord integration is an official Activity plus a bot.** Client mods (BetterDiscord/Vencord) break Discord's ToS.
- **Native host in Rust**, reusing this repo's signaling server and input wire format (`client/src/input.js`) unchanged.

### Next, in order
1. Install ViGEmBus and verify a real game sees the pad (and a real game is captured).
2. **Discord bot** slash commands (`/take`, `/pass @user`, `/release`), and rooms tied to the voice channel instead of codes.
3. **Installer** (Tauri): bundle ViGEmBus, code signing, auto-update, TURN relay on a small VPS.
4. **Discord test (last).** Create the app, tunnel with `cloudflared`, open the Activity, and record what the Diagnostics panel shows. If WebRTC or the Gamepad API is blocked, switch friends to a browser link the bot posts.

### Windows setup checklist
1. **Node.js 22+**: https://nodejs.org
2. **Visual Studio Build Tools** with the "Desktop development with C++" workload (Rust's MSVC linker needs it). Install this before Rust.
3. **Rust**: https://rustup.rs (default `x86_64-pc-windows-msvc` toolchain).
4. **cloudflared**: `winget install Cloudflare.cloudflared`
5. **ViGEmBus** driver, for step 3: https://github.com/nefarius/ViGEmBus/releases
6. In the project folder (on the external drive, likely `E:\Projects\couchplay`):
   ```powershell
   Remove-Item -Recurse -Force node_modules   # Linux build tools won't run on Windows
   npm install
   npm run build
   npm start          # then in a second terminal: npm run smoke; npm run e2e
   ```
7. Copy `.env.example` to `.env` and fill it in **before** `npm run build` (for the Discord test).

### Gotchas we already hit
- A D3D11 video-processor input view fails with E_INVALIDARG on a shader-resource-only texture. The capture copy texture needs `BIND_RENDER_TARGET` too.
- The NVENC Media Foundation encoder rejects `AVEncMPVDefaultBPictureCount` (it has no B-frames in low-latency mode anyway).
- `webrtc` 0.21 is the new sans-IO API (`PeerConnectionBuilder`, `poll()` loops for data channels), not the older callback API many examples online use.
- WASAPI loopback of a device is captured *after* the Windows volume stage: at 6% volume the stream was ~1/180 as loud. Undone with the endpoint's dB level (`undo_master_volume`). Process loopback is captured pre-volume, so it needs no compensation.
- `windows::PROPVARIANT` frees its blob on drop. Pointing it at a stack struct (process-loopback activation params) corrupts the heap and kills the process with no message. Wrap it in `ManuallyDrop`.
- e2e tests: never `taskkill /IM powershell.exe` (kills the user's shells); and `'C:\Windows\...'` in a JS string loses its backslashes, so use forward slashes.
- Puppeteer's `headless: false` Chrome got no real window in this session (no top-level window to capture), so the window e2e uses a PowerShell WinForms window instead.
- WGC only delivers frames when the window content changes, so a static window produces almost no capture callbacks; the encoder re-encodes the last frame on its own 60 fps clock.
- The keyboard-fallback controls must ignore key presses in text fields (fixed in `client/src/input.js`).
- Background tabs pause `requestAnimationFrame`. The host page sits behind the game, so it uses timers instead, and the e2e test polls by interval.
- Without `VITE_DISCORD_CLIENT_ID` set at build time, Vite strips out all the Discord code.
