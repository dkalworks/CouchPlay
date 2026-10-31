import './style.css';
import { Diagnostics } from './diagnostics.js';
import { inDiscord, setupDiscord } from './discord.js';
import { FLAG_KEYBOARD, InputSampler, gamepadApiStatus, pack } from './input.js';
import { ICE_SERVERS, Signaling, connectionStats, serialQueue } from './net.js';
import { PadView } from './padview.js';

const $ = (s) => document.querySelector(s);
const diag = new Diagnostics($('#diag'));
const padView = new PadView($('#pad'));
const sampler = new InputSampler();
const video = $('#video');

let sig = null;
let pc = null;
let inputCh = null;
let ctrlCh = null;
let myId = null;
let controller = null;
let seq = 0;
const enqueueSignal = serialQueue();

// ---------- startup ----------

async function main() {
  const gp = gamepadApiStatus();
  diag.set('Gamepad API', gp.detail, gp.ok);

  let name = localStorage.getItem('couchplay.name') || '';
  if (inDiscord()) {
    diag.set('Discord', 'signing in…');
    try {
      const d = await setupDiscord();
      name = d.name;
      diag.set('Discord', `signed in as ${d.name}`, true);
    } catch (e) {
      diag.set('Discord', e.message, false);
    }
  } else {
    diag.set('Discord', 'browser mode (not inside Discord)');
  }

  const room = new URLSearchParams(location.search).get('room') || localStorage.getItem('couchplay.room') || '';
  $('#room').value = room;
  $('#name').value = name;
  if (room && name) join(room, name);
}

$('#join-form').addEventListener('submit', (e) => {
  e.preventDefault();
  join($('#room').value, $('#name').value);
});

async function join(room, name) {
  room = room.trim().toUpperCase();
  name = name.trim();
  localStorage.setItem('couchplay.room', room);
  localStorage.setItem('couchplay.name', name);
  showError('');
  $('#join-form button').disabled = true;

  sig = new Signaling()
    .on('joined', (m) => {
      myId = m.id;
      $('#join').hidden = true;
      $('#stage').hidden = false;
      diag.set('Room', m.room, true);
    })
    .on('error', (m) => {
      showError(m.error);
      $('#join-form button').disabled = false;
      sig.close();
    })
    .on('state', renderState)
    .on('signal', (m) => enqueueSignal(() => onSignal(m.data)))
    .on('room-closed', () => leave('The host ended the stream.'))
    .on('close', () => diag.set('Signaling', 'disconnected', false));

  try {
    await sig.open();
    diag.set('Signaling', 'connected', true);
    sig.send({ t: 'join', room, name });
  } catch (e) {
    diag.set('Signaling', e.message, false);
    showError(e.message);
    $('#join-form button').disabled = false;
  }
}

function leave(reason) {
  pc?.close();
  pc = inputCh = ctrlCh = null;
  sig?.close();
  video.srcObject = null;
  $('#stage').hidden = true;
  $('#join').hidden = false;
  $('#join-form button').disabled = false;
  showError(reason);
}

function showError(text) {
  $('#error').textContent = text;
  $('#error').hidden = !text;
}

// ---------- WebRTC ----------

function createPc() {
  pc = new RTCPeerConnection({ iceServers: ICE_SERVERS });
  pc.onicecandidate = (e) => {
    if (e.candidate) sig.send({ t: 'signal', to: 'host', data: { candidate: e.candidate.toJSON() } });
  };
  pc.onconnectionstatechange = () =>
    diag.set('WebRTC', pc.connectionState, pc.connectionState === 'connected' ? true : pc.connectionState === 'failed' ? false : null);
  pc.ontrack = (e) => {
    // Ask the browser to play frames as soon as they decode instead of buffering for smoothness.
    if ('jitterBufferTarget' in e.receiver) e.receiver.jitterBufferTarget = 0;
    else e.receiver.playoutDelayHint = 0;
    const stream = e.streams[0] ?? (video.srcObject ||= new MediaStream());
    if (!e.streams[0]) stream.addTrack(e.track);
    video.srcObject = stream;
    video.play().catch(() => {});
  };
  pc.ondatachannel = ({ channel }) => {
    if (channel.label === 'input') {
      inputCh = channel;
      channel.onopen = () => diag.set('Input channel', 'open', true);
      channel.onclose = () => diag.set('Input channel', 'closed', false);
    } else if (channel.label === 'ctrl') {
      ctrlCh = channel;
      channel.onmessage = (e) => {
        const m = JSON.parse(e.data);
        if (m.t === 'pong') diag.set('Ping (data channel)', `${(performance.now() - m.ts).toFixed(1)} ms`, true);
      };
    }
  };
}

async function onSignal(data) {
  if (data.sdp) {
    if (data.sdp.type === 'offer') {
      pc?.close();
      createPc();
    }
    await pc.setRemoteDescription(data.sdp);
    if (data.sdp.type === 'offer') {
      await pc.setLocalDescription(await pc.createAnswer());
      sig.send({ t: 'signal', to: 'host', data: { sdp: pc.localDescription } });
    }
  } else if (data.candidate && pc) {
    await pc.addIceCandidate(data.candidate);
  }
}

// ---------- control token ----------

function renderState({ controller: c, peers }) {
  controller = c;
  const holder = c ? peers.find((p) => p.id === c)?.name ?? 'someone' : 'the host';
  const mine = c === myId;
  $('#holder').textContent = mine ? 'You have the controller 🎮' : `🎮 ${holder} is playing`;
  $('#holder').classList.toggle('mine', mine);
  $('#toggle').textContent = mine ? 'Pass back to host' : 'Take control';
  $('#peers').textContent = `Watching: ${peers.map((p) => p.name).join(', ')}`;
}

$('#toggle').addEventListener('click', () => sig.send({ t: controller === myId ? 'release' : 'take' }));
$('#unmute').addEventListener('click', () => {
  video.muted = !video.muted;
  $('#unmute').textContent = video.muted ? '🔇 Unmute' : '🔊 Mute';
});
// Clicking the stream gives the frame keyboard focus, which matters inside Discord's iframe.
video.addEventListener('click', () => window.focus());

// ---------- loops ----------

let lastPadId = null;
setInterval(() => {
  const { state, source } = sampler.sample();
  padView.render(state);

  const pad = sampler.gamepad();
  if (pad?.id !== lastPadId) {
    lastPadId = pad?.id ?? null;
    diag.set('Controller', pad ? `${pad.id} (${pad.mapping || 'non-standard mapping'})` : 'none, keyboard fallback', !!pad);
  }

  if (controller === myId && inputCh?.readyState === 'open') {
    inputCh.send(pack(++seq, state, source === 'keyboard' ? FLAG_KEYBOARD : 0));
  }
}, 8);

let lastBytes = 0;
let lastJitter = { delay: 0, count: 0 };
setInterval(async () => {
  if (ctrlCh?.readyState === 'open') ctrlCh.send(JSON.stringify({ t: 'ping', ts: performance.now() }));
  if (!pc) return;

  const s = await connectionStats(pc);
  if (s.route) diag.set('Route', s.route, !s.route.includes('relay') || null);
  if (s.rttMs != null) diag.set('Network RTT', `${s.rttMs.toFixed(1)} ms`, s.rttMs < 60);
  const v = s.inbound;
  if (v) {
    const kbps = ((v.bytesReceived - lastBytes) * 8) / 1000;
    lastBytes = v.bytesReceived;
    diag.set('Video', `${v.frameWidth ?? '?'}×${v.frameHeight ?? '?'} @ ${v.framesPerSecond ?? 0} fps, ${(kbps / 1000).toFixed(1)} Mbps, ${s.codec ?? ''}`, true);
    const d = v.jitterBufferDelay - lastJitter.delay;
    const n = v.jitterBufferEmittedCount - lastJitter.count;
    lastJitter = { delay: v.jitterBufferDelay, count: v.jitterBufferEmittedCount };
    if (n > 0) diag.set('Jitter buffer', `${((d / n) * 1000).toFixed(1)} ms`, d / n < 0.03);
  }
}, 1000);

main();
