import { apiBase } from './discord.js';

export const ICE_SERVERS = [{ urls: 'stun:stun.l.google.com:19302' }];
if (import.meta.env.VITE_TURN_URL) {
  ICE_SERVERS.push({
    urls: import.meta.env.VITE_TURN_URL,
    username: import.meta.env.VITE_TURN_USERNAME,
    credential: import.meta.env.VITE_TURN_CREDENTIAL,
  });
}

/** JSON-over-WebSocket signaling client with per-message-type handlers. */
export class Signaling {
  #ws = null;
  #handlers = new Map();

  on(type, fn) {
    this.#handlers.set(type, fn);
    return this;
  }

  open() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    this.#ws = new WebSocket(`${proto}//${location.host}${apiBase()}/api/ws`);
    this.#ws.onmessage = (e) => {
      const msg = JSON.parse(e.data);
      this.#handlers.get(msg.t)?.(msg);
    };
    this.#ws.onclose = () => this.#handlers.get('close')?.();
    return new Promise((resolve, reject) => {
      this.#ws.onopen = () => resolve();
      this.#ws.onerror = () => reject(new Error('Could not reach the CouchPlay server'));
    });
  }

  send(msg) {
    if (this.#ws?.readyState === WebSocket.OPEN) this.#ws.send(JSON.stringify(msg));
  }

  close() {
    this.#ws?.close();
  }
}

/** Runs async signal handlers one at a time so ICE candidates never beat their SDP. */
export function serialQueue() {
  let chain = Promise.resolve();
  return (fn) => {
    chain = chain.then(fn).catch((e) => console.error(e));
  };
}

/** Pulls the numbers we care about out of RTCPeerConnection.getStats(). */
export async function connectionStats(pc) {
  const report = await pc.getStats();
  const out = {};
  let pairId;
  report.forEach((s) => {
    if (s.type === 'transport' && s.selectedCandidatePairId) pairId = s.selectedCandidatePairId;
  });
  report.forEach((s) => {
    if (!pairId && s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded') pairId = s.id;
    if (s.type === 'inbound-rtp' && s.kind === 'video') out.inbound = s;
    if (s.type === 'outbound-rtp' && s.kind === 'video') out.outbound = s;
  });
  const pair = pairId && report.get(pairId);
  if (pair) {
    out.rttMs = pair.currentRoundTripTime != null ? pair.currentRoundTripTime * 1000 : null;
    const local = report.get(pair.localCandidateId);
    const remote = report.get(pair.remoteCandidateId);
    out.route = `${local?.candidateType ?? '?'} ⇄ ${remote?.candidateType ?? '?'} (${local?.protocol ?? '?'})`;
  }
  const codecId = out.inbound?.codecId ?? out.outbound?.codecId;
  if (codecId) out.codec = report.get(codecId)?.mimeType;
  return out;
}
