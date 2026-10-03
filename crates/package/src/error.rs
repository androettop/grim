use std::fmt;

/// Parse error, always carrying the absolute file offset where it happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub offset: usize,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ErrorKind {
    UnexpectedEof { wanted: usize, available: usize },
    BadMagic(u32),
    UnsupportedVersion(u16),
    CompactIndexOverflow,
    NameIndexOutOfRange(i64),
    ObjectRefOutOfRange(i32),
    Invalid(String),
    /// A bounded region was parsed and bytes were left over or missing.
    LengthMismatch { context: String, expected: usize, consumed: usize },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(offset: usize, kind: ErrorKind) -> Self {
        Self { offset, kind }
    }

    pub fn invalid(offset: usize, msg: impl Into<String>) -> Self {
        Self::new(offset, ErrorKind::Invalid(msg.into()))
    }

    pub fn context(self, ctx: impl fmt::Display) -> Self {
        let kind = match self.kind {
            ErrorKind::Invalid(m) => ErrorKind::Invalid(format!("{ctx}: {m}")),
            other => ErrorKind::Invalid(format!("{ctx}: {other}")),
        };
        Self { offset: self.offset, kind }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnexpectedEof { wanted, available } => {
                write!(f, "unexpected end of data: wanted {wanted} bytes, {available} left")
            }
            ErrorKind::BadMagic(m) => write!(f, "bad package magic {m:#010x}"),
            ErrorKind::UnsupportedVersion(v) => write!(f, "unsupported package version {v}"),
            ErrorKind::CompactIndexOverflow => write!(f, "compact index longer than 5 bytes"),
            ErrorKind::NameIndexOutOfRange(i) => write!(f, "name index out of range: {i}"),
            ErrorKind::ObjectRefOutOfRange(i) => write!(f, "object reference out of range: {i}"),
            ErrorKind::Invalid(m) => f.write_str(m),
            ErrorKind::LengthMismatch { context, expected, consumed } => write!(
                f,
                "{context}: expected {expected} bytes, consumed {consumed}"
            ),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{:#x}: {}", self.offset, self.kind)
    }
}

impl std::error::Error for Error {}
