//! HTTP range requests for file downloads (RFC 9110, section 14): a client
//! that lost the connection half-way through a large file asks for the rest
//! with `Range: bytes=<offset>-` and `If-Range: "<sha256>"` instead of
//! starting over.
//!
//! Only what resuming needs is supported: one range per request (a list of
//! ranges is answered with the whole file, which RFC 9110 allows), suffix
//! ranges (`bytes=-500`) and `If-Range` with the strong ETag of the content
//! (`"<sha256>"`, the same for every version with that content). A date in
//! `If-Range` never matches, so the client gets the whole file.

/// What to send for a download request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeDecision {
    /// No usable range (none asked for, several, malformed, or `If-Range`
    /// did not match): `200` with the whole file.
    Full,
    /// `206 Partial Content` with the bytes `start..=end`.
    Partial { start: u64, end: u64 },
    /// `416 Range Not Satisfiable`: the range starts at or after the end.
    Unsatisfiable,
}

/// The strong ETag of a file's content.
pub fn etag_of(sha256: &str) -> String {
    format!("\"{sha256}\"")
}

/// Decides how to answer a request with the headers `range` and `if_range`
/// for a file of `size` bytes whose ETag is `etag`.
pub fn decide(range: Option<&str>, if_range: Option<&str>, etag: &str, size: u64) -> RangeDecision {
    let Some(range) = range else {
        return RangeDecision::Full;
    };
    // If-Range: the range only applies to exactly this representation
    if let Some(validator) = if_range {
        if validator.trim() != etag {
            return RangeDecision::Full;
        }
    }
    let Some(spec) = range.trim().strip_prefix("bytes=") else {
        return RangeDecision::Full;
    };
    if spec.contains(',') {
        return RangeDecision::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeDecision::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // suffix range: the last N bytes
        let Ok(n) = last.parse::<u64>() else {
            return RangeDecision::Full;
        };
        if n == 0 || size == 0 {
            return RangeDecision::Unsatisfiable;
        }
        let start = size.saturating_sub(n);
        return RangeDecision::Partial {
            start,
            end: size - 1,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return RangeDecision::Full;
    };
    let end = if last.is_empty() {
        size.saturating_sub(1)
    } else {
        match last.parse::<u64>() {
            Ok(e) if e >= start => e.min(size.saturating_sub(1)),
            _ => return RangeDecision::Full,
        }
    };
    if start >= size {
        return RangeDecision::Unsatisfiable;
    }
    RangeDecision::Partial { start, end }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "\"abc\"";

    #[test]
    fn without_a_range_the_whole_file() {
        assert_eq!(decide(None, None, TAG, 100), RangeDecision::Full);
        assert_eq!(decide(None, Some(TAG), TAG, 100), RangeDecision::Full);
    }

    #[test]
    fn open_and_closed_ranges() {
        assert_eq!(
            decide(Some("bytes=40-"), None, TAG, 100),
            RangeDecision::Partial { start: 40, end: 99 }
        );
        assert_eq!(
            decide(Some("bytes=0-9"), None, TAG, 100),
            RangeDecision::Partial { start: 0, end: 9 }
        );
        // an end beyond the file is cut to the last byte
        assert_eq!(
            decide(Some("bytes=90-500"), None, TAG, 100),
            RangeDecision::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            decide(Some("bytes=-10"), None, TAG, 100),
            RangeDecision::Partial { start: 90, end: 99 }
        );
        assert_eq!(
            decide(Some("bytes=-500"), None, TAG, 100),
            RangeDecision::Partial { start: 0, end: 99 }
        );
    }

    #[test]
    fn past_the_end_is_unsatisfiable() {
        assert_eq!(
            decide(Some("bytes=100-"), None, TAG, 100),
            RangeDecision::Unsatisfiable
        );
        assert_eq!(
            decide(Some("bytes=0-"), None, TAG, 0),
            RangeDecision::Unsatisfiable
        );
        assert_eq!(
            decide(Some("bytes=-0"), None, TAG, 100),
            RangeDecision::Unsatisfiable
        );
    }

    #[test]
    fn if_range_must_name_this_content() {
        assert_eq!(
            decide(Some("bytes=40-"), Some(TAG), TAG, 100),
            RangeDecision::Partial { start: 40, end: 99 }
        );
        assert_eq!(
            decide(Some("bytes=40-"), Some("\"other\""), TAG, 100),
            RangeDecision::Full,
            "the file changed: start over"
        );
        assert_eq!(
            decide(
                Some("bytes=40-"),
                Some("Wed, 21 Oct 2015 07:28:00 GMT"),
                TAG,
                100
            ),
            RangeDecision::Full,
            "dates are not supported"
        );
    }

    #[test]
    fn unsupported_or_malformed_ranges_get_the_whole_file() {
        for range in [
            "bytes=0-9,20-29",
            "items=0-9",
            "bytes=x-",
            "bytes=9-3",
            "bytes=",
            "bytes=-x",
        ] {
            assert_eq!(
                decide(Some(range), None, TAG, 100),
                RangeDecision::Full,
                "{range}"
            );
        }
        assert_eq!(etag_of("ab12"), "\"ab12\"");
    }
}
