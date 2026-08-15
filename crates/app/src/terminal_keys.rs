//! Turning key presses into the bytes a terminal program is waiting for.
//!
//! A full-screen program does not read lines; it reads keys, and it reads them
//! as the byte sequences the DEC terminals used. Arrow keys are three bytes,
//! `Ctrl+C` is one, and the arrows change shape depending on a mode the program
//! itself sets. Sending the wrong thing does not fail loudly — it puts a stray
//! `A` in the buffer where a cursor movement should have been.
//!
//! Every sequence here is the xterm one, because that is what `TERM` claims and
//! what terminfo will therefore have told the program to expect.

use eframe::egui;

/// Bytes to send for a key press, or `None` for a key with no meaning here.
///
/// `application_cursor` is the mode the program sets with `CSI ? 1 h`, which
/// readline and every full-screen program does.
#[must_use]
pub(crate) fn encode(
    key: egui::Key,
    modifiers: egui::Modifiers,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    use egui::Key;

    let ctrl = modifiers.ctrl || modifiers.command;
    let alt = modifiers.alt;
    let shift = modifiers.shift;

    // Ctrl with a letter is a control code: Ctrl+A is 1, Ctrl+C is 3, and so
    // on up to Ctrl+Z. This is the one people notice immediately, because
    // Ctrl+C is how you stop something.
    if ctrl
        && !alt
        && let Some(code) = control_code(key)
    {
        return Some(vec![code]);
    }

    let bytes = match key {
        Key::Enter => vec![b'\r'],
        // DEL rather than BS. Every Unix shell and readline expects 0x7f for
        // the backspace key; 0x08 is what Ctrl+H sends and is treated as a
        // different key entirely.
        Key::Backspace => vec![0x7f],
        Key::Tab if shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![b'\t'],
        Key::Escape => vec![0x1b],

        Key::ArrowUp => cursor_key(b'A', modifiers, application_cursor),
        Key::ArrowDown => cursor_key(b'B', modifiers, application_cursor),
        Key::ArrowRight => cursor_key(b'C', modifiers, application_cursor),
        Key::ArrowLeft => cursor_key(b'D', modifiers, application_cursor),
        Key::Home => cursor_key(b'H', modifiers, application_cursor),
        Key::End => cursor_key(b'F', modifiers, application_cursor),

        Key::PageUp => tilde_key(5, modifiers),
        Key::PageDown => tilde_key(6, modifiers),
        Key::Insert => tilde_key(2, modifiers),
        Key::Delete => tilde_key(3, modifiers),

        // F1 to F4 are the "SS3" forms; F5 upwards use the tilde forms, with a
        // gap in the numbering that is historical and has to be spelled out.
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => tilde_key(15, modifiers),
        Key::F6 => tilde_key(17, modifiers),
        Key::F7 => tilde_key(18, modifiers),
        Key::F8 => tilde_key(19, modifiers),
        Key::F9 => tilde_key(20, modifiers),
        Key::F10 => tilde_key(21, modifiers),
        Key::F11 => tilde_key(23, modifiers),
        Key::F12 => tilde_key(24, modifiers),

        _ => return None,
    };
    Some(with_alt(bytes, alt))
}

/// The control code a letter produces with Ctrl held.
fn control_code(key: egui::Key) -> Option<u8> {
    use egui::Key;
    let code = match key {
        Key::A => 1,
        Key::B => 2,
        Key::C => 3,
        Key::D => 4,
        Key::E => 5,
        Key::F => 6,
        Key::G => 7,
        Key::H => 8,
        Key::I => 9,
        Key::J => 10,
        Key::K => 11,
        Key::L => 12,
        Key::M => 13,
        Key::N => 14,
        Key::O => 15,
        Key::P => 16,
        Key::Q => 17,
        Key::R => 18,
        Key::S => 19,
        Key::T => 20,
        Key::U => 21,
        Key::V => 22,
        Key::W => 23,
        Key::X => 24,
        Key::Y => 25,
        Key::Z => 26,
        // Ctrl+Space sends NUL, which is how programs are told to set a mark.
        Key::Space => 0,
        Key::OpenBracket => 27,
        Key::Backslash => 28,
        Key::CloseBracket => 29,
        _ => return None,
    };
    Some(code)
}

/// An arrow, Home or End, in whichever form the program has asked for.
fn cursor_key(final_byte: u8, modifiers: egui::Modifiers, application: bool) -> Vec<u8> {
    // With a modifier held the sequence always takes its parameterised form,
    // whatever the cursor mode: there is nowhere to put the modifier in the
    // three-byte one.
    if let Some(code) = modifier_code(modifiers) {
        return format!("\x1b[1;{code}")
            .into_bytes()
            .into_iter()
            .chain([final_byte])
            .collect();
    }
    if application {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// The `ESC [ n ~` family: page up and down, insert, delete, and F5 upwards.
fn tilde_key(number: u8, modifiers: egui::Modifiers) -> Vec<u8> {
    match modifier_code(modifiers) {
        Some(code) => format!("\x1b[{number};{code}~").into_bytes(),
        None => format!("\x1b[{number}~").into_bytes(),
    }
}

/// xterm's modifier parameter: 1 plus a bit each for shift, alt and ctrl.
///
/// `None` when nothing is held, because the unmodified sequences are shorter
/// and are what a program with a minimal terminfo entry will recognise.
fn modifier_code(modifiers: egui::Modifiers) -> Option<u8> {
    let mut code = 1;
    if modifiers.shift {
        code += 1;
    }
    if modifiers.alt {
        code += 2;
    }
    if modifiers.ctrl || modifiers.command {
        code += 4;
    }
    (code > 1).then_some(code)
}

/// Alt is sent as an Escape before the rest, which is how every terminal has
/// done it since the meta key stopped existing.
fn with_alt(bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if !alt {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(0x1b);
    out.extend(bytes);
    out
}

/// Typed text, ready to send.
///
/// Separate from [`encode`] because egui delivers text as `Event::Text` after
/// the platform has applied the keyboard layout, dead keys and any input
/// method — none of which a key code can be turned back into.
#[must_use]
pub(crate) fn encode_text(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Pasted text, wrapped in the markers a program asked for.
///
/// Bracketed paste is how a program tells pasted text from typed text — an
/// editor uses it to avoid auto-indenting what you pasted, and a shell uses it
/// to avoid running a pasted newline before you have looked at it.
#[must_use]
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    // Carriage returns, because that is what the Enter key sends and a paste
    // has to look like typing.
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return body.into_bytes();
    }
    let mut out = b"\x1b[200~".to_vec();
    out.extend(body.as_bytes());
    out.extend(b"\x1b[201~");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Key, Modifiers};

    fn plain(key: Key) -> Option<Vec<u8>> {
        encode(key, Modifiers::NONE, false)
    }

    #[test]
    fn enter_sends_a_carriage_return() {
        assert_eq!(plain(Key::Enter), Some(b"\r".to_vec()));
    }

    /// The one that is wrong in half of all hand-rolled terminals. Shells and
    /// readline expect DEL; 0x08 is Ctrl+H and means something else.
    #[test]
    fn backspace_sends_del_not_backspace() {
        assert_eq!(plain(Key::Backspace), Some(vec![0x7f]));
    }

    #[test]
    fn the_arrows_send_the_ordinary_sequences_by_default() {
        assert_eq!(plain(Key::ArrowUp), Some(b"\x1b[A".to_vec()));
        assert_eq!(plain(Key::ArrowDown), Some(b"\x1b[B".to_vec()));
        assert_eq!(plain(Key::ArrowRight), Some(b"\x1b[C".to_vec()));
        assert_eq!(plain(Key::ArrowLeft), Some(b"\x1b[D".to_vec()));
    }

    /// The mode readline and every full-screen program sets. Getting this
    /// wrong puts a stray `A` in the buffer instead of moving the cursor.
    #[test]
    fn application_cursor_mode_changes_the_arrows() {
        assert_eq!(
            encode(Key::ArrowUp, Modifiers::NONE, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode(Key::Home, Modifiers::NONE, true),
            Some(b"\x1bOH".to_vec())
        );
    }

    /// How you stop a runaway program, and the first thing anyone tries.
    #[test]
    fn ctrl_c_sends_the_interrupt() {
        assert_eq!(encode(Key::C, Modifiers::CTRL, false), Some(vec![3]));
    }

    #[test]
    fn the_control_letters_map_to_their_codes() {
        for (key, code) in [(Key::A, 1u8), (Key::D, 4), (Key::L, 12), (Key::Z, 26)] {
            assert_eq!(
                encode(key, Modifiers::CTRL, false),
                Some(vec![code]),
                "{key:?}"
            );
        }
        // Ctrl+Space is NUL, which is how a mark gets set.
        assert_eq!(encode(Key::Space, Modifiers::CTRL, false), Some(vec![0]));
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(
            encode(Key::Enter, Modifiers::ALT, false),
            Some(b"\x1b\r".to_vec())
        );
    }

    /// With a modifier held there is nowhere to put it in the three-byte form,
    /// so the sequence takes its parameterised shape whatever the mode.
    #[test]
    fn modified_arrows_take_the_parameterised_form() {
        assert_eq!(
            encode(Key::ArrowRight, Modifiers::CTRL, false),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            encode(Key::ArrowLeft, Modifiers::SHIFT, false),
            Some(b"\x1b[1;2D".to_vec())
        );
        assert_eq!(
            encode(Key::ArrowUp, Modifiers::CTRL, true),
            Some(b"\x1b[1;5A".to_vec()),
            "even in application mode"
        );
    }

    #[test]
    fn the_page_and_edit_keys_use_the_tilde_forms() {
        assert_eq!(plain(Key::PageUp), Some(b"\x1b[5~".to_vec()));
        assert_eq!(plain(Key::PageDown), Some(b"\x1b[6~".to_vec()));
        assert_eq!(plain(Key::Delete), Some(b"\x1b[3~".to_vec()));
        assert_eq!(plain(Key::Insert), Some(b"\x1b[2~".to_vec()));
    }

    #[test]
    fn shift_tab_sends_a_back_tab() {
        assert_eq!(
            encode(Key::Tab, Modifiers::SHIFT, false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(plain(Key::Tab), Some(b"\t".to_vec()));
    }

    /// The numbering has a gap after F4 that is historical and cannot be
    /// derived, so it is worth pinning.
    #[test]
    fn the_function_keys_use_their_historical_numbers() {
        assert_eq!(plain(Key::F1), Some(b"\x1bOP".to_vec()));
        assert_eq!(plain(Key::F4), Some(b"\x1bOS".to_vec()));
        assert_eq!(plain(Key::F5), Some(b"\x1b[15~".to_vec()));
        assert_eq!(plain(Key::F6), Some(b"\x1b[17~".to_vec()), "16 is skipped");
        assert_eq!(plain(Key::F12), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn a_key_with_no_terminal_meaning_sends_nothing() {
        assert_eq!(plain(Key::F20), None);
    }

    /// A pasted newline has to look like the Enter key, or a shell sees a line
    /// that never ends.
    #[test]
    fn paste_turns_newlines_into_carriage_returns() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb".to_vec());
        assert_eq!(encode_paste("a\r\nb", false), b"a\rb".to_vec());
    }

    #[test]
    fn bracketed_paste_wraps_the_text_in_its_markers() {
        assert_eq!(
            encode_paste("ls", true),
            b"\x1b[200~ls\x1b[201~".to_vec(),
            "so the program can tell this from typing"
        );
    }
}
