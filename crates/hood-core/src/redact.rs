//! Keep provider secrets out of logs and error texts (task 038).
//!
//! Provider endpoints carry the API key in the URL: Alchemy in the path
//! (`/v2/<key>`), others in the query or in `user:password@`. Anything that
//! prints an endpoint goes through [`redact_url`]; any foreign error text that
//! may embed the endpoint (e.g. a `reqwest::Error` source chain) goes through
//! [`scrub_url`] before it reaches a log line or an error message.
//!
//! Mirrored in `.claude/skills/feed-audit/scripts/feed_audit.py` (`redact_url` /
//! `scrub_url`, stdlib-only, runs on the server). Change both and their tests
//! together; the test vectors are shared: `src/redact_vectors.tsv`.

/// Replacement for every hidden part.
pub const MASK: &str = "***";

/// Parts of `scheme://[userinfo@]host[:port][/path][?query][#fragment]`.
struct Parts<'a> {
    scheme: &'a str,
    userinfo: Option<&'a str>,
    host: &'a str,
    /// Path, query and fragment together, as written (may be empty).
    rest: &'a str,
}

/// RFC 3986 scheme: `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`. Rejects values
/// like `h.io/v2/KEY?r=https://x`, where the first `://` sits inside the query.
fn is_scheme(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn split(url: &str) -> Option<Parts<'_>> {
    let (scheme, after) = url.split_once("://")?;
    if !is_scheme(scheme) {
        return None;
    }
    // `\` ends the authority too: the `url` crate (so reqwest) reads it as a
    // path separator for http(s)/ws(s).
    let end = after.find(['/', '\\', '?', '#']).unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    Some(Parts { scheme, userinfo, host, rest })
}

/// Scheme and host (with port) only; userinfo is dropped, a non-trivial path,
/// query or fragment becomes `/***` (an empty path or `/` is kept, so a URL
/// without secrets prints unchanged). Text that is not `scheme://…` is
/// replaced by `***` as a whole: it cannot be told which part is secret.
///
/// `https://x.g.alchemy.com/v2/KEY` -> `https://x.g.alchemy.com/***`;
/// `wss://feed.example.com` and `https://h/` -> unchanged.
#[must_use]
pub fn redact_url(url: &str) -> String {
    match split(url.trim()) {
        Some(p) if !p.host.is_empty() => {
            if p.rest.is_empty() || p.rest == "/" {
                format!("{}://{}{}", p.scheme, p.host, p.rest)
            } else {
                format!("{}://{}/{MASK}", p.scheme, p.host)
            }
        }
        _ => MASK.to_owned(),
    }
}

/// `text` with every occurrence of `url` replaced by `redact_url(url)`
/// and, as a second line of defence, every occurrence of its secret-bearing
/// parts (userinfo; path+query+fragment unless it is empty or `/`) replaced by
/// `***`. Use on error texts of third-party libraries that may embed the URL.
/// A very short userinfo (e.g. `u`) over-masks the text; that is safe, only
/// less readable.
#[must_use]
pub fn scrub_url(text: &str, url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return text.to_owned();
    }
    let mut out = text.replace(url, &redact_url(url));
    // Not a URL: the whole value is the secret and was replaced above.
    if let Some(p) = split(url) {
        let secrets = [p.userinfo.unwrap_or(""), if p.rest == "/" { "" } else { p.rest }];
        for s in secrets.into_iter().filter(|s| !s.is_empty()) {
            out = out.replace(s, MASK);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared with the Python copy (`test_feed_audit.py`).
    const VECTORS: &str = include_str!("redact_vectors.tsv");

    #[test]
    fn shared_vectors() {
        let mut n = 0;
        for line in VECTORS.lines().filter(|l| !l.starts_with('#')) {
            let (input, want) = line.split_once('\t').expect("input<TAB>expected");
            assert_eq!(redact_url(input), want, "{input:?}");
            n += 1;
        }
        assert!(n >= 15, "vectors file looks truncated: {n}");
    }

    #[test]
    fn scrub_removes_url_and_its_secret_parts() {
        let url = "https://u:PW@x.example.com/v2/SECRETKEY?k=Q";
        let text = format!("error sending request for url ({url}): refused; path /v2/SECRETKEY?k=Q; auth u:PW");
        let s = scrub_url(&text, url);
        assert!(!s.contains("SECRETKEY") && !s.contains("PW") && !s.contains("k=Q"), "{s}");
        assert!(s.contains("https://x.example.com/***"), "{s}");
        // Nothing to hide in a bare host URL; the text is kept.
        assert_eq!(scrub_url("GET https://h.io/ failed", "https://h.io/"), "GET https://h.io/ failed");
        assert_eq!(scrub_url("x", ""), "x");
        assert_eq!(scrub_url("bad value SECRET", "SECRET"), "bad value ***");
    }
}
