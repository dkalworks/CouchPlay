// End-to-end check of signaling and control-token rules against a running server.
// Usage: npm start (in another terminal), then npm run smoke
import assert from 'node:assert/strict';
import WebSocket from 'ws';
import { neutralState, pack, unpack } from '../client/src/input.js';

const URL = process.env.SMOKE_URL ?? 'ws://localhost:3001/api/ws';

function client() {
  const ws = new WebSocket(URL);
  const inbox = [];
  const waiters = [];
  ws.on('message', (raw) => {
    const msg = JSON.parse(raw);
    const i = waiters.findIndex((w) => w.pred(msg));
    if (i >= 0) waiters.splice(i, 1)[0].resolve(msg);
    else inbox.push(msg);
  });
  const next = (t, pred = () => true) => {
    const match = (m) => m.t === t && pred(m);
    const i = inbox.findIndex(match);
    if (i >= 0) return Promise.resolve(inbox.splice(i, 1)[0]);
    return new Promise((resolve, reject) => {
      waiters.push({ pred: match, resolve });
      setTimeout(() => reject(new Error(`timed out waiting for ${t}`)), 2000);
    });
  };
  return {
    ready: new Promise((r) => ws.once('open', r)),
    send: (m) => ws.send(JSON.stringify(m)),
    next,
    close: () => ws.close(),
  };
}

// Packet codec round trip.
const s = { ...neutralState(), buttons: (1 << 0) | (1 << 16), lx: -1, ry: 0.5, lt: 1 };
const u = unpack(pack(42, s, 1));
assert.equal(u.seq, 42);
assert.equal(u.flags, 1);
assert.equal(u.buttons, s.buttons);
assert.equal(u.lx, -1);
assert.ok(Math.abs(u.ry - 0.5) < 1e-3);
assert.equal(u.lt, 1);
assert.equal(unpack(new ArrayBuffer(3)), null);
console.log('✓ input packet round trip');

const host = client();
await host.ready;
host.send({ t: 'host' });
const { room } = await host.next('room');
assert.match(room, /^[A-Z2-9]{5}$/);
console.log(`✓ host created room ${room}`);

const bad = client();
await bad.ready;
bad.send({ t: 'join', room: 'ZZZZZ', name: 'x' });
await bad.next('error');
bad.close();
console.log('✓ unknown room rejected');

const alice = client();
const bob = client();
await Promise.all([alice.ready, bob.ready]);
alice.send({ t: 'join', room: room.toLowerCase(), name: 'Alice' });
const a = await alice.next('joined');
const pj = await host.next('peer-joined', (m) => m.id === a.id);
assert.equal(pj.name, 'Alice');
bob.send({ t: 'join', room, name: 'Bob' });
const b = await bob.next('joined');
await host.next('peer-joined', (m) => m.id === b.id);
console.log('✓ viewers joined (code is case-insensitive)');

host.send({ t: 'signal', to: a.id, data: { sdp: 'offer' } });
assert.deepEqual((await alice.next('signal')).data, { sdp: 'offer' });
alice.send({ t: 'signal', to: b.id, data: { sdp: 'answer' } }); // `to` ignored: viewers only reach host
const relayed = await host.next('signal');
assert.equal(relayed.from, a.id);
console.log('✓ signaling relayed host ⇄ viewer only');

alice.send({ t: 'take' });
assert.equal((await host.next('state', (m) => m.controller === a.id)).controller, a.id);
bob.send({ t: 'release' }); // Bob doesn't hold it: no effect
bob.send({ t: 'give', to: b.id }); // only the host may give
bob.send({ t: 'take' });
await bob.next('state', (m) => m.controller === b.id);
console.log('✓ take / release / give rules');

host.send({ t: 'give', to: a.id });
await alice.next('state', (m) => m.controller === a.id);
alice.close();
const after = await host.next('state', (m) => m.peers.length === 1);
assert.equal(after.controller, null);
console.log('✓ controller leaving returns control to host');

host.close();
await bob.next('room-closed');
bob.close();
console.log('✓ host leaving closes the room\n\nAll smoke tests passed.');
process.exit(0);
