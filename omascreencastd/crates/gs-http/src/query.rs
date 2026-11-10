//! Percent-decoding for query strings.
//!
//! Only the two sequences the protocol actually uses are decoded: `%XX` and
//! `+`. A malformed escape is passed through literally rather than rejected,
//! because a launch parameter the host does not act on must not be able to
//! break a request.

/// Decode `%XX` escapes and `+` in a query component.
#[must_use]
pub fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high << 4) | low);
                        index += 3;
                    }
                    _ => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_escapes_and_plus() {
        assert_eq!(decode("a%20b"), "a b");
        assert_eq!(decode("x+y"), "x y");
        assert_eq!(decode("%2F"), "/");
        assert_eq!(decode("plain"), "plain");
    }

    #[test]
    fn leaves_malformed_escapes_alone() {
        assert_eq!(decode("%"), "%");
        assert_eq!(decode("%2"), "%2");
        assert_eq!(decode("%zz"), "%zz");
    }

    #[test]
    fn decodes_utf8_sequences() {
        assert_eq!(decode("%E2%9C%93"), "✓");
    }
}
