//! RFC 1123 single-label hostnames for `.localhost` authorities and S3 buckets.

/// A validated RFC 1123 label: lowercase `[a-z0-9-]`, alphanumeric edges, `1..=63`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DnsLabel<'a>(&'a str);

/// Why a value is not a valid hostname label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    TooShort { min: usize },
    TooLong { max: usize },
    InvalidCharacter(char),
    EdgeHyphen,
    IpLiteral,
}

impl<'a> DnsLabel<'a> {
    /// Validates a `domain`, `cdn`, or route-prefix label (`1..=63`).
    pub fn new(value: &'a str) -> Result<Self, DnsError> {
        validate(value, 1, 63, false)
    }

    /// Validates an `s3` bucket name (`3..=63`, never an IPv4 literal).
    pub fn bucket(value: &'a str) -> Result<Self, DnsError> {
        validate(value, 3, 63, true)
    }

    /// The validated label text.
    #[must_use]
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl DnsError {
    /// A single-line description naming the rejected value.
    #[must_use]
    pub fn message(self, value: &str) -> String {
        match self {
            Self::TooShort { min } => format!("`{value}` is shorter than {min} characters"),
            Self::TooLong { max } => format!("`{value}` is longer than {max} characters"),
            Self::InvalidCharacter(character) => format!(
                "`{value}` contains `{character}`; hostname labels use lowercase ASCII `[a-z0-9-]`"
            ),
            Self::EdgeHyphen => {
                format!("`{value}` must start and end with a lowercase letter or digit")
            }
            Self::IpLiteral => format!("`{value}` is an IPv4 address, not a bucket name"),
        }
    }

    /// The concrete remedy shown after `next:`.
    #[must_use]
    pub fn next(self, slot: &str) -> String {
        match self {
            Self::IpLiteral => {
                format!("pick a bucket name that is not an IPv4 address for `{slot}`")
            }
            Self::TooShort { min } => {
                format!("use at least {min} lowercase letters, digits, or `-` for `{slot}`")
            }
            _ => format!(
                "use 1 to 63 lowercase letters, digits, or `-` for `{slot}`, starting and ending with a letter or digit"
            ),
        }
    }
}

fn validate(value: &str, min: usize, max: usize, bucket: bool) -> Result<DnsLabel<'_>, DnsError> {
    if value.len() < min {
        return Err(DnsError::TooShort { min });
    }
    if value.len() > max {
        return Err(DnsError::TooLong { max });
    }
    if bucket && is_ipv4(value) {
        return Err(DnsError::IpLiteral);
    }
    for character in value.chars() {
        if !(character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-') {
            return Err(DnsError::InvalidCharacter(character));
        }
    }
    if value.starts_with('-') || value.ends_with('-') {
        return Err(DnsError::EdgeHyphen);
    }
    Ok(DnsLabel(value))
}

fn is_ipv4(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.chars().all(|character| character.is_ascii_digit())
                && part.parse::<u8>().is_ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_accept_hyphens_and_reject_non_rfc1123_shapes() {
        assert_eq!(
            DnsLabel::new("static-cdn").expect("valid").as_str(),
            "static-cdn"
        );
        assert_eq!(
            DnsLabel::bucket("assets").expect("valid").as_str(),
            "assets"
        );
        assert!(DnsLabel::new(&["static", "cdn"].join("_")).is_err());
        assert!(DnsLabel::new("Static").is_err());
        assert!(DnsLabel::new("-cdn").is_err());
        assert!(DnsLabel::new("cdn-").is_err());
        assert!(DnsLabel::new(&"a".repeat(64)).is_err());
        assert!(DnsLabel::bucket("ab").is_err());
        assert!(DnsLabel::bucket("1.2.3.4").is_err());
    }
}
