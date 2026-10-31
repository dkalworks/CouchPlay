# 🛋️ CouchPlay

Stream your game to friends in a Discord voice channel and pass the controller around.

**Status: step 1, the feasibility spike.** It answers the riskiest question before we build the native host:

> Can a **Discord Activity** receive low-latency **WebRTC video** and read a **controller** (Gamepad API), and can we hand control between friends cleanly?

The host side runs in a browser tab for now (screen capture via `getDisplayMedia`, Chrome's encoder). In later steps it becomes a native Rust app with NVENC and a real virtual gamepad.

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

_Last updated: 2026-10-31. Read this first when picking the project up on a new machine._

### Done
- Step 1 spike, working and tested in a browser (`npm run smoke`, `npm run e2e` both pass).
  The signaling server, control-token arbiter, host and viewer pages, input wire format, Discord SDK sign-in code and diagnostics panel are all in place.

### Not done / unverified
- **Never run inside Discord yet.** Needs a Discord developer app (see "Run it inside Discord"). The two unknowns:
  1. Does WebRTC video connect inside the Activity iframe? (Diagnostics → WebRTC / Route)
  2. Does the Gamepad API work there? (Diagnostics → Gamepad API / Controller)
- No TURN server, so friends on strict NATs may fail to connect.
- Input only reaches the on-screen "Virtual controller" panel, not the game.

### Decisions made
- **Main dev platform: Windows.** That's where users' games and the installer are, DXGI/WGC capture needs no permission prompts, and ViGEm gives games a real Xbox pad. Linux (PipeWire + uinput) may come later.
- **Friends need zero installs**: Discord Activity, or a browser link if the Activity sandbox blocks something. That's why it uses WebRTC instead of Moonlight.
- **Discord integration is an official Activity plus a bot.** Client mods (BetterDiscord/Vencord) break Discord's ToS.
- **Native host in Rust**, reusing this repo's signaling server and input wire format (`client/src/input.js`) unchanged.

### Next, in order
1. **Discord test.** Create the app, tunnel with `cloudflared`, open the Activity, and record what the Diagnostics panel shows. If WebRTC or the Gamepad API is blocked, switch friends to a browser link the bot posts.
2. **Native host, Windows (Rust)** in a new `host/` folder:
   capture (Windows Graphics Capture) → NVENC H.264 (low-latency preset, no B-frames) → WebRTC (`webrtc-rs`), with the same signaling protocol as `client/src/host.js`.
3. **Virtual gamepad**: decode the 22-byte input packets → ViGEmBus Xbox 360 pad, with a neutral reset on handoff.
4. **Discord bot** slash commands (`/take`, `/pass @user`, `/release`), and rooms tied to the voice channel instead of codes.
5. **Installer** (Tauri): bundle ViGEmBus, code signing, auto-update, TURN relay on a small VPS.

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
- The keyboard-fallback controls must ignore key presses in text fields (fixed in `client/src/input.js`).
- Background tabs pause `requestAnimationFrame`. The host page sits behind the game, so it uses timers instead, and the e2e test polls by interval.
- Without `VITE_DISCORD_CLIENT_ID` set at build time, Vite strips out all the Discord code.
