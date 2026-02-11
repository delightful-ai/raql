use camino::Utf8PathBuf;
use text_size::{TextRange, TextSize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RaqlFileId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SrcSpan {
    pub file: RaqlFileId,
    pub range: TextRange,
}

impl SrcSpan {
    #[must_use]
    pub fn new(file: RaqlFileId, start: u32, end: u32) -> Self {
        Self {
            file,
            range: TextRange::new(TextSize::from(start), TextSize::from(end)),
        }
    }

    #[must_use]
    pub fn from_range(file: RaqlFileId, range: TextRange) -> Self {
        Self { file, range }
    }

    #[must_use]
    pub fn merge(self, other: SrcSpan) -> SrcSpan {
        debug_assert_eq!(self.file, other.file);
        SrcSpan {
            file: self.file,
            range: TextRange::new(
                self.range.start().min(other.range.start()),
                self.range.end().max(other.range.end()),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spanned<T> {
    pub span: SrcSpan,
    pub value: T,
}

impl<T> Spanned<T> {
    #[must_use]
    pub fn new(span: SrcSpan, value: T) -> Self {
        Self { span, value }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    id: RaqlFileId,
    path: Utf8PathBuf,
    text: String,
    include_stack: Box<[Utf8PathBuf]>,
}

impl SourceFile {
    #[must_use]
    pub fn id(&self) -> RaqlFileId {
        self.id
    }

    #[must_use]
    pub fn path(&self) -> &Utf8PathBuf {
        &self.path
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn include_stack(&self) -> &[Utf8PathBuf] {
        &self.include_stack
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    #[must_use]
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }

    pub(crate) fn add_file(
        &mut self,
        path: Utf8PathBuf,
        text: String,
        include_stack: Box<[Utf8PathBuf]>,
    ) -> RaqlFileId {
        let id = RaqlFileId(self.files.len() as u32);
        self.files.push(SourceFile {
            id,
            path,
            text,
            include_stack,
        });
        id
    }

    #[must_use]
    pub fn get(&self, id: RaqlFileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }
}
