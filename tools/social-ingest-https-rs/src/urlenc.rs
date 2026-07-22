//! `urllib.parse.urlencode` twin — exact query-string parity with the Python
//! adapters (§67: hand-rolled, ~30 lines, instead of another dependency).
//!
//! Python's `urlencode` runs each value through `quote_plus`: bytes in
//! `[A-Za-z0-9_.~-]` pass through, space becomes `+`, everything else
//! (including `/` and non-ASCII UTF-8 bytes) becomes uppercase `%XX`. The
//! twitterapi/tiktok twins rely on this for query, cursor and hashtag
//! parameters; matching it byte-for-byte means the vendor sees the identical
//! request URL from either lane. Pure functions (§22).

/// `quote_plus` for one value.
#[must_use]
pub fn quote_plus(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'~' | b'-' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}

/// `urllib.parse.urlencode` for a key/value slice (keys are the literal
/// parameter names the Python twins use — already URL-safe).
#[must_use]
pub fn urlencode(params: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (n, (k, v)) in params.iter().enumerate() {
        if n > 0 {
            out.push('&');
        }
        out.push_str(&quote_plus(k));
        out.push('=');
        out.push_str(&quote_plus(v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_quote_plus() {
        // python3: urllib.parse.quote_plus('($SOL OR "pump.fun") -is:retweet')
        assert_eq!(
            quote_plus(r#"($SOL OR "pump.fun") -is:retweet"#),
            "%28%24SOL+OR+%22pump.fun%22%29+-is%3Aretweet"
        );
        assert_eq!(quote_plus("from:a OR from:b"), "from%3Aa+OR+from%3Ab");
        assert_eq!(quote_plus("safe_.~-AZ09"), "safe_.~-AZ09");
    }

    #[test]
    fn utf8_percent_encodes_per_byte() {
        // python3: urllib.parse.quote_plus('gm🚀') == 'gm%F0%9F%9A%80'
        assert_eq!(quote_plus("gm\u{1F680}"), "gm%F0%9F%9A%80");
    }

    #[test]
    fn urlencode_joins_pairs() {
        assert_eq!(
            urlencode(&[("query", "$SOL x"), ("queryType", "Latest"), ("cursor", "")]),
            "query=%24SOL+x&queryType=Latest&cursor="
        );
    }
}
