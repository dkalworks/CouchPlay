//! Wire format shared with `client/src/input.js`: 22 bytes, little-endian.
//!   u8 version | u8 flags | u16 reserved | u32 seq | u32 buttons
//!   i16 lx, ly, rx, ry | u8 lt, rt

pub const PACKET_VERSION: u8 = 1;
pub const PACKET_SIZE: usize = 22;

/// Bit positions follow the standard Gamepad API button order.
pub mod button {
    pub const A: u32 = 1 << 0;
    pub const B: u32 = 1 << 1;
    pub const X: u32 = 1 << 2;
    pub const Y: u32 = 1 << 3;
    pub const LB: u32 = 1 << 4;
    pub const RB: u32 = 1 << 5;
    pub const BACK: u32 = 1 << 8;
    pub const START: u32 = 1 << 9;
    pub const LS: u32 = 1 << 10;
    pub const RS: u32 = 1 << 11;
    pub const UP: u32 = 1 << 12;
    pub const DOWN: u32 = 1 << 13;
    pub const LEFT: u32 = 1 << 14;
    pub const RIGHT: u32 = 1 << 15;
    pub const GUIDE: u32 = 1 << 16;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadState {
    pub buttons: u32,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u8,
    pub rt: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub flags: u8,
    pub seq: u32,
    pub state: PadState,
}

pub fn unpack(buf: &[u8]) -> Option<Packet> {
    if buf.len() != PACKET_SIZE || buf[0] != PACKET_VERSION {
        return None;
    }
    let u32_at = |i: usize| u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
    let i16_at = |i: usize| i16::from_le_bytes(buf[i..i + 2].try_into().unwrap());
    Some(Packet {
        flags: buf[1],
        seq: u32_at(4),
        state: PadState {
            buttons: u32_at(8),
            lx: i16_at(12),
            ly: i16_at(14),
            rx: i16_at(16),
            ry: i16_at(18),
            lt: buf[20],
            rt: buf[21],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Produced by `pack(7, { buttons: A|Start, lx: 1, ly: -1, rx: 0, ry: 0.5, lt: 0, rt: 1 })`
    // in client/src/input.js.
    const SAMPLE: [u8; 22] = [
        1, 0, 0, 0, 7, 0, 0, 0, 0x01, 0x02, 0, 0, 0xff, 0x7f, 0x01, 0x80, 0, 0, 0x00, 0x40, 0, 255,
    ];

    #[test]
    fn decodes_js_packet() {
        let p = unpack(&SAMPLE).unwrap();
        assert_eq!(p.seq, 7);
        assert_eq!(p.state.buttons, button::A | button::START);
        assert_eq!((p.state.lx, p.state.ly), (32767, -32767));
        assert_eq!((p.state.rx, p.state.ry), (0, 16384));
        assert_eq!((p.state.lt, p.state.rt), (0, 255));
    }

    #[test]
    fn rejects_bad_packets() {
        assert!(unpack(&SAMPLE[..21]).is_none());
        let mut wrong_version = SAMPLE;
        wrong_version[0] = 2;
        assert!(unpack(&wrong_version).is_none());
    }
}
