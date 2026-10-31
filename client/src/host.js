import './style.css';
import { Diagnostics } from './diagnostics.js';
import { neutralState, unpack } from './input.js';
import { ICE_SERVERS, Signaling, connectionStats, serialQueue } from './net.js';
import { PadView } from './padview.js';

const $ = (s) => document.querySelector(s);
const diag = new Diagnostics($('#diag'));
const padView = new PadView($('#pad'));

let stream = null;
let sig = null;
let controller = null;
let roster = [];
/** id -> { name, pc, lastSeq, enqueue } */
const peers = new Map();

// Stand-in for the virtual controller. The native host will feed this into ViGEm/uinput.
let pad = neutralState();
let padDirty = true;
let accepted = 0;
let dropped = 0;

// ---------- start / stop ----------

$('#start').addEventListener('click', start);
$('#stop').addEventListener('click', stop);
$('#bitrate').addEventListener('change', () => {
  for (const p of peers.values()) applyBitrate(p.pc);
});
$('#copy').addEventListener('click', () => navigator.clipboard.writeText($('#link').href));

async function start() {
  try {
    stream = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: { ideal: 60 }, width: { ideal: 1920 }, height: { ideal: 1080 } },
      audio: true,
    });
  } catch (e) {
    diag.set('Capture', e.message, false);
    return;
  }
  const [videoTrack] = stream.getVideoTracks();
  videoTrack.contentHint = 'motion';
  videoTrack.addEventListener('ended', stop);
  $('#preview').srcObject = stream;
  diag.set('Capture', `${videoTrack.getSettings().width}×${videoTrack.getSettings().height}, audio: ${stream.getAudioTracks().length ? 'yes' : 'no'}`, true);

  sig = new Signaling()
    .on('room', (m) => {
      const link = `${location.origin}/?room=${m.room}`;
      $('#code').textContent = m.room;
      $('#link').href = link;
      $('#link').textContent = link;
    })
    .on('state', onState)
    .on('peer-joined', (m) => createPeer(m.id, m.name))
    .on('peer-left', (m) => closePeer(m.id))
    .on('signal', (m) => {
      const p = peers.get(m.from);
      p?.enqueue(() => onSignal(p, m.data));
    })
    .on('close', () => diag.set('Signaling', 'disconnected', false));

  try {
    await sig.open();
  } catch (e) {
    diag.set('Signaling', e.message, false);
    return stop();
  }
  diag.set('Signaling', 'connected', true);
  sig.send({ t: 'host' });

  $('#setup').hidden = true;
  $('#live').hidden = false;
}

function stop() {
  for (const id of [...peers.keys()]) closePeer(id);
  stream?.getTracks().forEach((t) => t.stop());
  stream = null;
  sig?.close();
  sig = null;
  $('#setup').hidden = false;
  $('#live').hidden = true;
}

// ---------- peers ----------

const kbps = () => Number($('#bitrate').value);

function preferCodec(transceiver, mimeType) {
  const codecs = RTCRtpSender.getCapabilities?.('video')?.codecs;
  if (!codecs || !transceiver.setCodecPreferences) return;
  const rank = (c) => (c.mimeType.toLowerCase() === mimeType.toLowerCase() ? 0 : 1);
  transceiver.setCodecPreferences([...codecs].sort((a, b) => rank(a) - rank(b)));
}

async function applyBitrate(pc) {
  for (const sender of pc.getSenders()) {
    if (sender.track?.kind !== 'video') continue;
    const params = sender.getParameters();
    if (!params.encodings?.length) continue;
    params.encodings[0].maxBitrate = kbps() * 1000;
    params.encodings[0].maxFramerate = 60;
    await sender.setParameters(params).catch((e) => console.warn('setParameters', e));
  }
}

async function createPeer(id, name) {
  const pc = new RTCPeerConnection({ iceServers: ICE_SERVERS });
  const peer = { name, pc, lastSeq: -1, enqueue: serialQueue() };
  peers.set(id, peer);

  for (const track of stream.getTracks()) {
    const tr = pc.addTransceiver(track, {
      direction: 'sendonly',
      streams: [stream],
      sendEncodings: track.kind === 'video' ? [{ maxBitrate: kbps() * 1000, maxFramerate: 60 }] : undefined,
    });
    // H.264 is the codec most likely to hit the GPU's hardware encoder.
    if (track.kind === 'video') preferCodec(tr, 'video/H264');
  }

  // Unordered + no retransmits: a late input packet is worse than a lost one.
  const input = pc.createDataChannel('input', { ordered: false, maxRetransmits: 0 });
  input.binaryType = 'arraybuffer';
  input.onmessage = (e) => onInput(id, peer, e.data);

  const ctrl = pc.createDataChannel('ctrl');
  ctrl.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.t === 'ping') ctrl.send(JSON.stringify({ t: 'pong', ts: m.ts }));
  };

  pc.onicecandidate = (e) => {
    if (e.candidate) sig.send({ t: 'signal', to: id, data: { candidate: e.candidate.toJSON() } });
  };
  pc.onconnectionstatechange = renderRoster;

  await pc.setLocalDescription(await pc.createOffer());
  sig.send({ t: 'signal', to: id, data: { sdp: pc.localDescription } });
  renderRoster();
}

function closePeer(id) {
  peers.get(id)?.pc.close();
  peers.delete(id);
  renderRoster();
}

async function onSignal(peer, data) {
  if (data.sdp) await peer.pc.setRemoteDescription(data.sdp);
  else if (data.candidate) await peer.pc.addIceCandidate(data.candidate);
}

// ---------- input arbitration ----------

function onInput(id, peer, buf) {
  if (id !== controller) {
    dropped++;
    return;
  }
  const p = unpack(buf);
  // Unordered channel: ignore anything older than what we've already applied.
  if (!p || p.seq <= peer.lastSeq) return;
  peer.lastSeq = p.seq;
  accepted++;
  pad = p;
  padDirty = true;
}

function onState(m) {
  if (m.controller !== controller) {
    // Hand-off: release everything so no button stays held across owners.
    pad = neutralState();
    padDirty = true;
    const p = peers.get(m.controller);
    if (p) p.lastSeq = -1;
  }
  controller = m.controller;
  roster = m.peers;
  renderRoster();
}

function renderRoster() {
  const holder = controller ? roster.find((p) => p.id === controller)?.name : null;
  $('#holder').textContent = holder ? `🎮 ${holder} is playing` : '🎮 You are playing';
  $('#take-back').hidden = !controller;

  const list = $('#roster');
  list.replaceChildren(
    ...roster.map((p) => {
      const li = document.createElement('li');
      const state = peers.get(p.id)?.pc.connectionState ?? 'pending';
      li.innerHTML = `<span class="name"></span><span class="conn ${state}">${state}</span>`;
      li.querySelector('.name').textContent = p.id === controller ? `🎮 ${p.name}` : p.name;
      if (p.id !== controller) {
        const btn = document.createElement('button');
        btn.textContent = 'Give control';
        btn.onclick = () => sig.send({ t: 'give', to: p.id });
        li.append(btn);
      }
      return li;
    }),
  );
  if (!roster.length) list.innerHTML = '<li class="muted">No one yet. Share the code in voice chat.</li>';
}

$('#take-back').addEventListener('click', () => sig.send({ t: 'give', to: null }));

// ---------- loops ----------

// A timer rather than requestAnimationFrame: the host tab usually sits behind the game.
setInterval(() => {
  if (padDirty) {
    padView.render(pad);
    padDirty = false;
  }
}, 16);

setInterval(async () => {
  if (!sig) return;
  diag.set('Input packets/s', `${accepted} accepted, ${dropped} dropped (not controller)`, accepted > 0 || null);
  accepted = dropped = 0;

  const p = peers.get(controller) ?? peers.values().next().value;
  if (!p) return;
  const s = await connectionStats(p.pc);
  if (s.codec) diag.set('Encoder', `${s.codec}${s.outbound?.encoderImplementation ? ` (${s.outbound.encoderImplementation})` : ''}`, true);
  if (s.outbound) diag.set('Send rate', `${s.outbound.framesPerSecond ?? 0} fps to ${p.name}`, true);
  if (s.route) diag.set(`Route to ${p.name}`, s.route);
}, 1000);
