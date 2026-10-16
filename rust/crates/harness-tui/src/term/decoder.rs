use std::io;

use super::{PROBE_LIMIT, Probe, ProbeReplies};
use crate::key_event::{
    InputEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};

/// Owns framing from the first startup byte through the last input byte.
/// Replies, including late/partial multiplexer replies, never become keys.
#[derive(Default)]
pub struct Decoder {
    probe: Probe,
    text: Vec<u8>,
    alt: bool,
    paste: Option<Vec<u8>>,
    cr: bool,
    events: Vec<InputEvent>,
}

impl Decoder {
    /// Terminal replies received so far. Capabilities can be cached at startup
    /// while this same decoder continues filtering replies for the session.
    pub fn replies(&self) -> &ProbeReplies {
        &self.probe.replies
    }

    /// Consume an arbitrary transport chunk. Drain typed events with `take_events`.
    pub fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        for &byte in bytes {
            if let Some(paste) = &mut self.paste {
                paste.push(byte);
                if paste.ends_with(b"\x1b[201~") {
                    paste.truncate(paste.len() - 6);
                    let text = String::from_utf8(std::mem::take(paste))
                        .map_err(|_| io::ErrorKind::InvalidData)?;
                    self.paste = None;
                    self.events.push(InputEvent::Paste(text));
                } else if paste.len() > PROBE_LIMIT + 6 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                continue;
            }
            self.probe.feed(&[byte]);
            for frame in std::mem::take(&mut self.probe.input) {
                self.input(&frame)?;
            }
        }
        Ok(())
    }

    /// Take only classified input; there is no raw startup replay API.
    pub fn take_events(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.events)
    }

    fn input(&mut self, frame: &[u8]) -> io::Result<()> {
        if frame == b"\x1b[200~" {
            self.paste = Some(Vec::new());
            return Ok(());
        }
        if frame == b"\x1b[I" {
            self.events.push(InputEvent::Focus(true));
            return Ok(());
        }
        if frame == b"\x1b[O" {
            self.events.push(InputEvent::Focus(false));
            return Ok(());
        }
        if let Some(mouse) = mouse(frame) {
            self.events.push(InputEvent::Mouse(mouse));
            return Ok(());
        }
        if frame.starts_with(b"\x1b[") || frame.starts_with(b"\x1bO") {
            if let Some(key) = decode_key(frame) {
                self.events.push(InputEvent::Key(key));
            }
            return Ok(());
        }
        // OSC/DCS/APC frames are terminal traffic, recognized or otherwise.
        if frame.starts_with(b"\x1b]") || frame.starts_with(b"\x1bP") || frame.starts_with(b"\x1b_")
        {
            return Ok(());
        }
        if frame == b"\x1b" {
            self.events.push(InputEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
            return Ok(());
        }
        let bytes = if let Some(bytes) = frame.strip_prefix(b"\x1b") {
            self.alt = true;
            bytes
        } else {
            frame
        };
        for &byte in bytes {
            self.text.push(byte);
            match std::str::from_utf8(&self.text) {
                Ok(text) => {
                    let mut keys = Vec::new();
                    for c in text.chars() {
                        if c == '\n' && self.cr {
                            self.cr = false;
                            continue;
                        }
                        self.cr = c == '\r';
                        let (code, modifiers) = match c {
                            '\r' | '\n' => (KeyCode::Enter, KeyModifiers::NONE),
                            '\t' => (KeyCode::Tab, KeyModifiers::NONE),
                            '\0' => (KeyCode::Null, KeyModifiers::NONE),
                            '\u{7f}' | '\u{8}' => (KeyCode::Backspace, KeyModifiers::NONE),
                            '\u{1}'..='\u{1a}' => (
                                KeyCode::Char(
                                    char::from_u32(u32::from(c) + 96)
                                        .ok_or(io::ErrorKind::InvalidData)?,
                                ),
                                KeyModifiers::CONTROL,
                            ),
                            _ => (KeyCode::Char(c), KeyModifiers::NONE),
                        };
                        keys.push(InputEvent::Key(KeyEvent::new(
                            code,
                            modifiers
                                | if self.alt {
                                    KeyModifiers::ALT
                                } else {
                                    KeyModifiers::NONE
                                },
                        )));
                    }
                    self.events.extend(keys);
                    self.text.clear();
                    self.alt = false;
                }
                Err(error) if error.error_len().is_none() && self.text.len() < 4 => {}
                Err(_) => {
                    self.text.clear();
                    self.alt = false;
                    return Err(io::ErrorKind::InvalidData.into());
                }
            }
        }
        Ok(())
    }
}

fn key_number(code: KeyCode) -> Option<u32> {
    Some(match code {
        KeyCode::Char(c) if !c.is_control() => u32::from(c),
        KeyCode::Null => 0,
        KeyCode::Enter => 13,
        KeyCode::Tab => 9,
        KeyCode::Esc => 27,
        KeyCode::Backspace => 127,
        KeyCode::Insert => 57348,
        KeyCode::Delete => 57349,
        KeyCode::Left => 57350,
        KeyCode::Right => 57351,
        KeyCode::Up => 57352,
        KeyCode::Down => 57353,
        KeyCode::PageUp => 57354,
        KeyCode::PageDown => 57355,
        KeyCode::Home => 57356,
        KeyCode::End => 57357,
        KeyCode::CapsLock => 57358,
        KeyCode::ScrollLock => 57359,
        KeyCode::NumLock => 57360,
        KeyCode::PrintScreen => 57361,
        KeyCode::Pause => 57362,
        KeyCode::Menu => 57363,
        KeyCode::F(n @ 1..=35) => 57363 + u32::from(n),
        _ => return None,
    })
}

fn numbered_key(number: u32) -> Option<KeyCode> {
    Some(match number {
        0 => KeyCode::Null,
        9 => KeyCode::Tab,
        13 => KeyCode::Enter,
        27 => KeyCode::Esc,
        127 => KeyCode::Backspace,
        57348 => KeyCode::Insert,
        57349 => KeyCode::Delete,
        57350 => KeyCode::Left,
        57351 => KeyCode::Right,
        57352 => KeyCode::Up,
        57353 => KeyCode::Down,
        57354 => KeyCode::PageUp,
        57355 => KeyCode::PageDown,
        57356 => KeyCode::Home,
        57357 => KeyCode::End,
        57358 => KeyCode::CapsLock,
        57359 => KeyCode::ScrollLock,
        57360 => KeyCode::NumLock,
        57361 => KeyCode::PrintScreen,
        57362 => KeyCode::Pause,
        57363 => KeyCode::Menu,
        57364..=57398 => KeyCode::F(u8::try_from(number - 57363).ok()?),
        _ => KeyCode::Char(char::from_u32(number).filter(|c| !c.is_control())?),
    })
}

/// Encode the representable kitty key-event domain, rejecting unsupported
/// physical-key metadata instead of silently changing the event's identity.
pub fn encode_key(key: KeyEvent) -> io::Result<Vec<u8>> {
    if key.state.contains(KeyEventState::KEYPAD) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let code = key_number(key.code).ok_or(io::ErrorKind::Unsupported)?;
    // Reserved kitty functional code points cannot also encode literal text.
    if matches!(key.code, KeyCode::Char(c) if (57344..=63743).contains(&u32::from(c))) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut modifiers = 0;
    for (flag, bit) in [
        (KeyModifiers::SHIFT, 1),
        (KeyModifiers::ALT, 2),
        (KeyModifiers::CONTROL, 4),
        (KeyModifiers::SUPER, 8),
        (KeyModifiers::HYPER, 16),
        (KeyModifiers::META, 32),
    ] {
        if key.modifiers.contains(flag) {
            modifiers |= bit;
        }
    }
    if key.state.contains(KeyEventState::CAPS_LOCK) {
        modifiers |= 64;
    }
    if key.state.contains(KeyEventState::NUM_LOCK) {
        modifiers |= 128;
    }
    let kind = match key.kind {
        KeyEventKind::Press => 1,
        KeyEventKind::Repeat => 2,
        KeyEventKind::Release => 3,
    };
    Ok(format!("\x1b[{code};{}:{kind}u", modifiers + 1).into_bytes())
}

fn modifiers(value: u16) -> Option<(KeyModifiers, KeyEventState)> {
    let flags = value.checked_sub(1)?;
    if flags > 255 {
        return None;
    }
    let mut modifiers = KeyModifiers::NONE;
    for (bit, flag) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ] {
        if flags & bit != 0 {
            modifiers |= flag;
        }
    }
    let mut state = KeyEventState::NONE;
    if flags & 64 != 0 {
        state |= KeyEventState::CAPS_LOCK;
    }
    if flags & 128 != 0 {
        state |= KeyEventState::NUM_LOCK;
    }
    Some((modifiers, state))
}

fn decode_key(frame: &[u8]) -> Option<KeyEvent> {
    let text = std::str::from_utf8(frame).ok()?;
    let body = text
        .strip_prefix("\x1b[")
        .or_else(|| text.strip_prefix("\x1bO"))?;
    let action = body.chars().last()?;
    let parameters = body.get(..body.len().checked_sub(1)?)?;
    let mut fields = parameters.split(';');
    let first = fields.next()?;
    let mut modifier = fields.next().unwrap_or("1").split(':');
    let (mut modifiers, state) = modifiers(modifier.next()?.parse().ok()?)?;
    let kind = match modifier.next().unwrap_or("1") {
        "1" => KeyEventKind::Press,
        "2" => KeyEventKind::Repeat,
        "3" => KeyEventKind::Release,
        _ => return None,
    };
    let code = match action {
        'u' => {
            if fields.next().is_some() {
                return None;
            }
            numbered_key(first.split(':').next()?.parse().ok()?)?
        }
        '~' if first == "27" => numbered_key(fields.next()?.parse().ok()?)?,
        '~' => match first {
            "2" => KeyCode::Insert,
            "3" => KeyCode::Delete,
            "5" => KeyCode::PageUp,
            "6" => KeyCode::PageDown,
            _ => return None,
        },
        'A' => KeyCode::Up,
        'B' => KeyCode::Down,
        'C' => KeyCode::Right,
        'D' => KeyCode::Left,
        'H' => KeyCode::Home,
        'F' => KeyCode::End,
        'P' => KeyCode::F(1),
        'Q' => KeyCode::F(2),
        'R' => KeyCode::F(3),
        'S' => KeyCode::F(4),
        'Z' => {
            modifiers |= KeyModifiers::SHIFT;
            KeyCode::BackTab
        }
        _ => return None,
    };
    Some(KeyEvent {
        code,
        modifiers,
        kind,
        state,
    })
}

fn mouse(frame: &[u8]) -> Option<MouseEvent> {
    let text = std::str::from_utf8(frame).ok()?.strip_prefix("\x1b[<")?;
    let release = text.ends_with('m');
    if !release && !text.ends_with('M') {
        return None;
    }
    let mut fields = text.get(..text.len().checked_sub(1)?)?.split(';');
    let flags: u16 = fields.next()?.parse().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    if fields.next().is_some() || flags > 255 {
        return None;
    }
    let mut modifiers = KeyModifiers::NONE;
    if flags & 4 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if flags & 8 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if flags & 16 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    let button = match flags & 3 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        _ => MouseButton::Right,
    };
    let kind = if flags & 64 != 0 {
        match flags & 3 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if release {
        MouseEventKind::Up(button)
    } else if flags & 32 != 0 {
        if flags & 3 == 3 {
            MouseEventKind::Moved
        } else {
            MouseEventKind::Drag(button)
        }
    } else {
        MouseEventKind::Down(button)
    };
    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}
