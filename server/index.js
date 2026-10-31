import express from 'express';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { WebSocketServer } from 'ws';
import { Rooms } from './rooms.js';

const PORT = Number(process.env.PORT) || 3001;
const dist = resolve(import.meta.dirname, '../dist');
const rooms = new Rooms();

const app = express();
app.use(express.json());

// Discord Activity OAuth: swap the code from the Embedded App SDK for an access token.
app.post('/api/token', async (req, res) => {
  const clientId = process.env.VITE_DISCORD_CLIENT_ID;
  const clientSecret = process.env.DISCORD_CLIENT_SECRET;
  if (!clientId || !clientSecret) {
    return res.status(500).json({ error: 'Discord credentials are not configured on the server' });
  }
  const { code } = req.body ?? {};
  if (typeof code !== 'string') return res.status(400).json({ error: 'Missing code' });

  const r = await fetch('https://discord.com/api/oauth2/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      client_id: clientId,
      client_secret: clientSecret,
      grant_type: 'authorization_code',
      code,
    }),
  });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) return res.status(502).json({ error: 'Token exchange failed', detail: body.error });
  res.json({ access_token: body.access_token });
});

app.get('/api/health', (_req, res) => res.json({ ok: true, rooms: rooms.count }));

app.use(express.static(dist));

const server = createServer(app);
const wss = new WebSocketServer({ server, path: '/api/ws' });

wss.on('connection', (ws) => {
  ws.isAlive = true;
  ws.on('pong', () => (ws.isAlive = true));
  rooms.attach(ws);
});

// Drop dead sockets so a vanished host closes its room, and keep proxies from idling us out.
const heartbeat = setInterval(() => {
  for (const ws of wss.clients) {
    if (!ws.isAlive) {
      ws.terminate();
      continue;
    }
    ws.isAlive = false;
    ws.ping();
  }
}, 15_000);
wss.on('close', () => clearInterval(heartbeat));

server.listen(PORT, () => {
  console.log(`CouchPlay server on http://localhost:${PORT}`);
});
