import { randomInt } from 'node:crypto';

// No 0/O/1/I so codes are easy to read out loud in voice chat.
const ALPHABET = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
const randomCode = (len) =>
  Array.from({ length: len }, () => ALPHABET[randomInt(ALPHABET.length)]).join('');

function send(ws, msg) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(msg));
}

/**
 * Signaling + control-token arbiter.
 *
 * A room has one host (the PC running the game) and any number of viewers.
 * `controller` is the viewer id currently allowed to drive the virtual pad,
 * or null when the host is playing. The host enforces it on the input it
 * receives; the server is the source of truth for who holds it.
 */
export class Rooms {
  #rooms = new Map();

  get count() {
    return this.#rooms.size;
  }

  attach(ws) {
    ws.ctx = { room: null, id: null };
    ws.on('message', (raw) => {
      let msg;
      try {
        msg = JSON.parse(raw);
      } catch {
        return;
      }
      if (msg && typeof msg.t === 'string') this.#handle(ws, msg);
    });
    ws.on('close', () => this.#leave(ws));
  }

  #handle(ws, msg) {
    const { room, id } = ws.ctx;
    if (!room) {
      if (msg.t === 'host') return this.#host(ws);
      if (msg.t === 'join') return this.#join(ws, msg);
      return send(ws, { t: 'error', error: 'Not in a room' });
    }

    const isHost = id === 'host';
    switch (msg.t) {
      case 'signal': {
        // Viewers may only talk to the host, and the host to its viewers.
        const target = isHost ? room.peers.get(msg.to)?.ws : room.host;
        return send(target, { t: 'signal', from: id, data: msg.data });
      }
      case 'take':
        if (!isHost) this.#setController(room, id);
        return;
      case 'release':
        if (room.controller === id) this.#setController(room, null);
        return;
      case 'give':
        if (isHost) this.#setController(room, room.peers.has(msg.to) ? msg.to : null);
        return;
    }
  }

  #host(ws) {
    let code;
    do code = randomCode(5);
    while (this.#rooms.has(code));

    const room = { code, host: ws, peers: new Map(), controller: null };
    this.#rooms.set(code, room);
    ws.ctx = { room, id: 'host' };
    send(ws, { t: 'room', room: code });
    this.#broadcastState(room);
  }

  #join(ws, msg) {
    const room = this.#rooms.get(String(msg.room ?? '').trim().toUpperCase());
    if (!room) return send(ws, { t: 'error', error: 'Room not found. Check the code with the host.' });

    const id = randomCode(8);
    const name = String(msg.name ?? '').trim().slice(0, 32) || `Guest ${room.peers.size + 1}`;
    room.peers.set(id, { ws, name });
    ws.ctx = { room, id };

    send(ws, { t: 'joined', id, room: room.code });
    send(room.host, { t: 'peer-joined', id, name });
    this.#broadcastState(room);
  }

  #setController(room, controller) {
    if (room.controller === controller) return;
    room.controller = controller;
    this.#broadcastState(room);
  }

  #broadcastState(room) {
    const state = {
      t: 'state',
      controller: room.controller,
      peers: [...room.peers].map(([id, p]) => ({ id, name: p.name })),
    };
    send(room.host, state);
    for (const p of room.peers.values()) send(p.ws, state);
  }

  #leave(ws) {
    const { room, id } = ws.ctx;
    if (!room) return;
    ws.ctx = { room: null, id: null };

    if (id === 'host') {
      this.#rooms.delete(room.code);
      for (const p of room.peers.values()) {
        p.ws.ctx = { room: null, id: null };
        send(p.ws, { t: 'room-closed' });
      }
      return;
    }

    room.peers.delete(id);
    if (room.controller === id) room.controller = null;
    send(room.host, { t: 'peer-left', id });
    this.#broadcastState(room);
  }
}
