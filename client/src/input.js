// Standard Gamepad API button order (https://w3c.github.io/gamepad/#remapping).
export const BUTTONS = [
  'A', 'B', 'X', 'Y', 'LB', 'RB', 'LT', 'RT', 'Back', 'Start',
  'LS', 'RS', 'Up', 'Down', 'Left', 'Right', 'Guide',
];

/**
 * Wire format, 22 bytes little-endian. The full pad state goes in every packet,
 * so the unreliable channel can drop packets without leaving buttons stuck.
 *   u8 version | u8 flags | u16 reserved | u32 seq | u32 buttons
 *   i16 lx, ly, rx, ry | u8 lt, rt
 */
export const PACKET_VERSION = 1;
export const PACKET_SIZE = 22;
export const FLAG_KEYBOARD = 1;

export const neutralState = () => ({ buttons: 0, lx: 0, ly: 0, rx: 0, ry: 0, lt: 0, rt: 0 });

const toI16 = (v) => Math.round(Math.max(-1, Math.min(1, v)) * 32767);
const toU8 = (v) => Math.round(Math.max(0, Math.min(1, v)) * 255);

export function pack(seq, s, flags = 0) {
  const buf = new ArrayBuffer(PACKET_SIZE);
  const v = new DataView(buf);
  v.setUint8(0, PACKET_VERSION);
  v.setUint8(1, flags);
  v.setUint32(4, seq, true);
  v.setUint32(8, s.buttons, true);
  v.setInt16(12, toI16(s.lx), true);
  v.setInt16(14, toI16(s.ly), true);
  v.setInt16(16, toI16(s.rx), true);
  v.setInt16(18, toI16(s.ry), true);
  v.setUint8(20, toU8(s.lt));
  v.setUint8(21, toU8(s.rt));
  return buf;
}

export function unpack(buf) {
  if (!(buf instanceof ArrayBuffer) || buf.byteLength !== PACKET_SIZE) return null;
  const v = new DataView(buf);
  if (v.getUint8(0) !== PACKET_VERSION) return null;
  return {
    flags: v.getUint8(1),
    seq: v.getUint32(4, true),
    buttons: v.getUint32(8, true),
    lx: v.getInt16(12, true) / 32767,
    ly: v.getInt16(14, true) / 32767,
    rx: v.getInt16(16, true) / 32767,
    ry: v.getInt16(18, true) / 32767,
    lt: v.getUint8(20) / 255,
    rt: v.getUint8(21) / 255,
  };
}

// Keyboard fallback so the spike stays usable if Discord blocks the Gamepad API.
const KEY_BUTTONS = {
  KeyZ: 0, KeyX: 1, KeyC: 2, KeyV: 3, KeyQ: 4, KeyE: 5, KeyR: 6, KeyF: 7,
  ShiftLeft: 8, Enter: 9, ArrowUp: 12, ArrowDown: 13, ArrowLeft: 14, ArrowRight: 15,
};
const DEADZONE = 0.08;
const dz = (v) => (Math.abs(v) < DEADZONE ? 0 : v);

/** Probes whether this frame may use the Gamepad API (Discord's iframe may forbid it). */
export function gamepadApiStatus() {
  if (!('getGamepads' in navigator)) return { ok: false, detail: 'not supported by this browser' };
  if (document.featurePolicy && !document.featurePolicy.allowsFeature('gamepad')) {
    return { ok: false, detail: 'blocked by permissions policy' };
  }
  try {
    navigator.getGamepads();
    return { ok: true, detail: 'available' };
  } catch (e) {
    return { ok: false, detail: `${e.name}: ${e.message}` };
  }
}

export class InputSampler {
  #keys = new Set();

  constructor(target = window) {
    target.addEventListener('keydown', (e) => {
      if (e.target instanceof Element && e.target.closest('input, textarea, select')) return;
      if (e.code in KEY_BUTTONS || /^Key[WASD]$/.test(e.code)) {
        this.#keys.add(e.code);
        e.preventDefault();
      }
    });
    target.addEventListener('keyup', (e) => this.#keys.delete(e.code));
    target.addEventListener('blur', () => this.#keys.clear());
  }

  /** First connected gamepad, or null. */
  gamepad() {
    try {
      return [...navigator.getGamepads()].find((g) => g?.connected) ?? null;
    } catch {
      return null;
    }
  }

  /** Merged state of the first gamepad and the keyboard. */
  sample() {
    const s = neutralState();
    let source = null;

    const pad = this.gamepad();
    if (pad) {
      source = 'gamepad';
      pad.buttons.forEach((b, i) => {
        if (b.pressed && i < 32) s.buttons |= 1 << i;
      });
      [s.lx, s.ly, s.rx, s.ry] = [0, 1, 2, 3].map((i) => dz(pad.axes[i] ?? 0));
      s.lt = pad.buttons[6]?.value ?? 0;
      s.rt = pad.buttons[7]?.value ?? 0;
    }

    if (this.#keys.size) {
      source ??= 'keyboard';
      for (const code of this.#keys) {
        if (code in KEY_BUTTONS) s.buttons |= 1 << KEY_BUTTONS[code];
      }
      if (this.#keys.has('KeyA')) s.lx = -1;
      if (this.#keys.has('KeyD')) s.lx = 1;
      if (this.#keys.has('KeyW')) s.ly = -1;
      if (this.#keys.has('KeyS')) s.ly = 1;
      if (this.#keys.has('KeyR')) s.lt = 1;
      if (this.#keys.has('KeyF')) s.rt = 1;
    }
    s.buttons >>>= 0;
    return { state: s, source };
  }
}
