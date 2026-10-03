/// Bytes we do not understand yet, kept with their absolute file offset (never discarded).
#[derive(Clone, PartialEq, Eq)]
pub struct Unknown {
    pub offset: usize,
    pub bytes: Vec<u8>,
}

impl Unknown {
    pub fn new(offset: usize, bytes: &[u8]) -> Self {
        Self { offset, bytes: bytes.to_vec() }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl std::fmt::Debug for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unknown({} bytes @ {:#x})", self.bytes.len(), self.offset)
    }
}
