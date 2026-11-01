use crate::input::{PadState, button};
use tracing::{info, warn};
use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};

/// XInput report values, ready to hand to the virtual Xbox 360 pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XState {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

/// Standard Gamepad API layout -> XInput. LT/RT arrive as analog values, so their button
/// bits are ignored. The Gamepad API has up = -1 on the sticks; XInput has up = +32767.
pub fn to_xinput(s: &PadState) -> XState {
    const MAP: [(u32, u16); 13] = [
        (button::A, XButtons::A),
        (button::B, XButtons::B),
        (button::X, XButtons::X),
        (button::Y, XButtons::Y),
        (button::LB, XButtons::LB),
        (button::RB, XButtons::RB),
        (button::BACK, XButtons::BACK),
        (button::START, XButtons::START),
        (button::LS, XButtons::LTHUMB),
        (button::RS, XButtons::RTHUMB),
        (button::UP, XButtons::UP),
        (button::DOWN, XButtons::DOWN),
        (button::LEFT, XButtons::LEFT),
    ];
    let mut buttons = MAP.iter().filter(|(bit, _)| s.buttons & bit != 0).fold(0, |acc, (_, x)| acc | x);
    if s.buttons & button::RIGHT != 0 {
        buttons |= XButtons::RIGHT;
    }
    if s.buttons & button::GUIDE != 0 {
        buttons |= XButtons::GUIDE;
    }
    XState {
        buttons,
        lt: s.lt,
        rt: s.rt,
        lx: s.lx,
        ly: s.ly.saturating_neg(),
        rx: s.rx,
        ry: s.ry.saturating_neg(),
    }
}

/// The virtual Xbox 360 controller games see. Without the ViGEmBus driver it still tracks
/// state, so the rest of the host keeps working and only the game input is missing.
pub struct VirtualPad {
    target: Option<Xbox360Wired<Client>>,
    current: XState,
}

impl VirtualPad {
    pub fn new() -> Self {
        let target = match Self::plug_in() {
            Ok(t) => {
                info!("virtual Xbox 360 controller plugged in");
                Some(t)
            }
            Err(e) => {
                warn!(
                    "no virtual controller ({e}); game input is disabled. \
                     Install ViGEmBus: https://github.com/nefarius/ViGEmBus/releases"
                );
                None
            }
        };
        Self { target, current: XState::default() }
    }

    fn plug_in() -> Result<Xbox360Wired<Client>, vigem_client::Error> {
        let client = Client::connect()?;
        let mut target = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
        target.plugin()?;
        target.wait_ready()?;
        Ok(target)
    }

    pub fn update(&mut self, state: &PadState) {
        self.apply(to_xinput(state));
    }

    /// Releases every button and centres the sticks; called on every handoff.
    pub fn reset(&mut self) {
        self.apply(XState::default());
    }

    fn apply(&mut self, x: XState) {
        self.current = x;
        let Some(target) = self.target.as_mut() else { return };
        let report = XGamepad {
            buttons: XButtons(x.buttons),
            left_trigger: x.lt,
            right_trigger: x.rt,
            thumb_lx: x.lx,
            thumb_ly: x.ly,
            thumb_rx: x.rx,
            thumb_ry: x.ry,
        };
        if let Err(e) = target.update(&report) {
            warn!("virtual controller update failed ({e}); disabling game input");
            self.target = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_buttons_and_flips_sticks() {
        let s = PadState {
            buttons: button::A | button::RIGHT | button::GUIDE | button::START,
            lx: 100,
            ly: -32767, // pushed up
            rx: -5,
            ry: 32767, // pushed down
            lt: 7,
            rt: 255,
        };
        let x = to_xinput(&s);
        assert_eq!(x.buttons, XButtons::A | XButtons::RIGHT | XButtons::GUIDE | XButtons::START);
        assert_eq!((x.lx, x.ly), (100, 32767));
        assert_eq!((x.rx, x.ry), (-5, -32767));
        assert_eq!((x.lt, x.rt), (7, 255));
    }

    #[test]
    fn trigger_button_bits_are_ignored() {
        let x = to_xinput(&PadState { buttons: (1 << 6) | (1 << 7), ..Default::default() });
        assert_eq!(x.buttons, 0);
    }

    #[test]
    fn neutral_stays_neutral() {
        assert_eq!(to_xinput(&PadState::default()), XState::default());
    }

    #[test]
    fn works_without_the_driver() {
        let mut pad = VirtualPad::new();
        pad.update(&PadState { buttons: button::A, ..Default::default() });
        pad.reset();
    }
}
