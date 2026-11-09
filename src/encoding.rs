//! Text encodings: detect on load, convert to UTF-8 for display, and write
//! back in the original encoding, BOM and line endings on save.

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

/// How a file was stored on disk, so saving can reproduce it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextFormat {
    pub encoding: &'static Encoding,
    pub bom: bool,
    pub crlf: bool,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self { encoding: UTF_8, bom: false, crlf: false }
    }
}

impl TextFormat {
    /// Short label for the status bar, e.g. `UTF-8`, `Shift_JIS`, `UTF-16LE BOM`.
    pub fn label(&self) -> String {
        let mut label = self.encoding.name().to_string();
        if self.bom {
            label.push_str(" BOM");
        }
        label
    }
}

pub struct Decoded {
    /// Always valid UTF-8 (invalid input is replaced with U+FFFD).
    pub utf8: Vec<u8>,
    pub encoding: &'static Encoding,
    pub bom: bool,
    /// Input contained bytes invalid for the detected encoding.
    pub lossy: bool,
}

/// Starts with a byte-order mark (UTF-8/16)? Such files contain NUL bytes in
/// UTF-16 and must not be mistaken for binary.
pub fn has_bom(bytes: &[u8]) -> bool {
    Encoding::for_bom(bytes).is_some()
}

pub fn decode(bytes: Vec<u8>) -> Decoded {
    if let Some((encoding, bom_len)) = Encoding::for_bom(&bytes) {
        let (text, lossy) = encoding.decode_without_bom_handling(&bytes[bom_len..]);
        return Decoded { utf8: text.into_owned().into_bytes(), encoding, bom: true, lossy };
    }
    if std::str::from_utf8(&bytes).is_ok() {
        // The common case: no copy.
        return Decoded { utf8: bytes, encoding: UTF_8, bom: false, lossy: false };
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(&bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);
    let (text, lossy) = encoding.decode_without_bom_handling(&bytes);
    Decoded { utf8: text.into_owned().into_bytes(), encoding, bom: false, lossy }
}

/// True if saving `utf8` in `encoding` reproduces `original` byte for byte.
/// Some legacy sequences decode cleanly but encode differently (Shift_JIS
/// `ED 40` comes back as `FA 5C`); such files must not be edited.
pub fn round_trips(original: &[u8], utf8: &str, encoding: &'static Encoding, bom: bool) -> bool {
    let (bytes, unmappable) = encode(utf8, TextFormat { encoding, bom, crlf: false });
    !unmappable && bytes == original
}

/// Converts editor text back to the file's line endings and encoding.
/// Returns the bytes and whether some characters could not be represented.
pub fn encode(text: &str, format: TextFormat) -> (Vec<u8>, bool) {
    let normalized;
    let text = if format.crlf {
        normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
        normalized.as_str()
    } else {
        text
    };
    let encoding = format.encoding;
    if encoding == UTF_16LE || encoding == UTF_16BE {
        // encoding_rs decodes UTF-16 but only encodes to UTF-8; do it by hand.
        let le = encoding == UTF_16LE;
        let mut out = Vec::with_capacity(text.len() * 2 + 2);
        let mut push = |unit: u16| out.extend(if le { unit.to_le_bytes() } else { unit.to_be_bytes() });
        if format.bom {
            push(0xFEFF);
        }
        text.encode_utf16().for_each(&mut push);
        return (out, false);
    }
    let mut out = Vec::with_capacity(text.len() + 3);
    if format.bom && encoding == UTF_8 {
        out.extend_from_slice(b"\xEF\xBB\xBF");
    }
    let (bytes, _, unmappable) = encoding.encode(text);
    out.extend_from_slice(&bytes);
    (out, unmappable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(original: &[u8]) {
        let decoded = decode(original.to_vec());
        let text = String::from_utf8(decoded.utf8).unwrap();
        let crlf = text.contains("\r\n");
        let format = TextFormat { encoding: decoded.encoding, bom: decoded.bom, crlf };
        let (bytes, unmappable) = encode(&text.replace("\r\n", "\n"), format);
        assert!(!unmappable);
        assert_eq!(bytes, original, "round trip through {}", decoded.encoding.name());
    }

    #[test]
    fn plain_utf8_is_not_copied_or_changed() {
        let d = decode(b"hello\n".to_vec());
        assert_eq!((d.encoding, d.bom, d.lossy), (UTF_8, false, false));
        round_trip(b"hello\nworld\n");
    }

    #[test]
    fn keeps_utf8_bom_and_crlf() {
        round_trip(b"\xEF\xBB\xBFa\r\nb\r\n");
    }

    #[test]
    fn utf16_with_bom_round_trips() {
        let mut le = vec![0xFF, 0xFE];
        "hi ü\r\n".encode_utf16().for_each(|u| le.extend(u.to_le_bytes()));
        assert!(has_bom(&le));
        let d = decode(le.clone());
        assert_eq!(String::from_utf8(d.utf8).unwrap(), "hi ü\r\n");
        round_trip(&le);
    }

    #[test]
    fn notices_sequences_that_do_not_round_trip() {
        // NEC-selected IBM extension in Shift_JIS: decodes, but encodes as FA 5C.
        let original = b"abc \xED\x40 def\n".to_vec();
        let (text, had_errors) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(&original);
        assert!(!had_errors, "decodes cleanly");
        assert!(!round_trips(&original, &text, encoding_rs::SHIFT_JIS, false));
        let clean = b"caf\xE9\n".to_vec();
        let d = decode(clean.clone());
        assert!(round_trips(&clean, std::str::from_utf8(&d.utf8).unwrap(), d.encoding, d.bom));
    }

    #[test]
    fn detects_legacy_encodings() {
        // "café crème" in Windows-1252, and Japanese in Shift_JIS.
        let latin = b"caf\xE9 cr\xE8me, d\xE9j\xE0 vu\n".to_vec();
        let d = decode(latin.clone());
        assert_eq!(d.encoding.name(), "windows-1252");
        assert_eq!(String::from_utf8(d.utf8).unwrap(), "café crème, déjà vu\n");
        round_trip(&latin);

        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("これは日本語のテキストです。文字コードの判定をテストします。\n");
        let d = decode(sjis.to_vec());
        assert_eq!(d.encoding.name(), "Shift_JIS");
        round_trip(&sjis);
    }
}
