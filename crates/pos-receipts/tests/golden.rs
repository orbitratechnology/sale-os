//! E13-T01 acceptance: a 3-line receipt encodes to the hand-verified golden bytes.

use pos_receipts::escpos::{Align, Cut, EscPos, decode};

const GOLDEN_BIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/escpos/three_line_receipt.bin"
);
const GOLDEN_TXT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/escpos/three_line_receipt.txt"
);

fn three_line_receipt() -> Vec<u8> {
    let mut p = EscPos::new();
    p.align(Align::Center)
        .bold(true)
        .line("CORNER SHOP")
        .bold(false)
        .align(Align::Left)
        .line("Cola 330ml        1.20")
        .bold(true)
        .line("TOTAL             1.20")
        .bold(false)
        .feed(3)
        .cut(Cut::Partial);
    p.into_bytes()
}

/// Every byte written out by hand from the Epson ESC/POS command reference.
const HAND_VERIFIED: &[u8] = &[
    0x1B, 0x40, // ESC @            initialise
    0x1B, 0x61, 0x01, // ESC a 1          align centre
    0x1B, 0x45, 0x01, // ESC E 1          bold on
    b'C', b'O', b'R', b'N', b'E', b'R', b' ', b'S', b'H', b'O', b'P', 0x0A, // text, LF
    0x1B, 0x45, 0x00, // ESC E 0          bold off
    0x1B, 0x61, 0x00, // ESC a 0          align left
    b'C', b'o', b'l', b'a', b' ', b'3', b'3', b'0', b'm', b'l', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
    b' ', b'1', b'.', b'2', b'0', 0x0A, 0x1B, 0x45, 0x01, // ESC E 1          bold on
    b'T', b'O', b'T', b'A', b'L', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
    b' ', b'1', b'.', b'2', b'0', 0x0A, 0x1B, 0x45, 0x00, // ESC E 0          bold off
    0x1B, 0x64, 0x03, // ESC d 3          feed 3 lines
    0x1D, 0x56, 0x42, 0x00, // GS V 66 0        feed to cutter, partial cut
];

#[test]
fn encoder_matches_hand_verified_bytes() {
    assert_eq!(three_line_receipt(), HAND_VERIFIED);
}

#[test]
fn golden_files_are_current() {
    let bytes = three_line_receipt();
    let text = decode(&bytes);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(GOLDEN_BIN, &bytes).unwrap();
        std::fs::write(GOLDEN_TXT, &text).unwrap();
    }
    assert_eq!(
        std::fs::read(GOLDEN_BIN).unwrap(),
        bytes,
        "run with UPDATE_GOLDENS=1 after reviewing the change"
    );
    assert_eq!(std::fs::read_to_string(GOLDEN_TXT).unwrap(), text);
}
