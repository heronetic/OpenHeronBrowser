use std::rc::Rc;

use boa_ast::LinearSpan;
use boa_gc::{Finalize, Trace};

thread_local! {
    /// OHB patch: set while compiling `eval` code (see `compacted`).
    pub(crate) static COMPACT_SOURCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// OHB patch: set while compiling the embedder's built-in scripts: their
    /// functions keep no source (they print as `[native code]`, like the
    /// browser's own functions, and a realm doesn't hold megabytes of text).
    pub(crate) static DROP_SOURCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// OHB patch: compile the following scripts as built-ins (no source text).
pub fn set_builtin_source(on: bool) {
    DROP_SOURCE.with(|c| c.set(on));
}

struct Inner {
    source_text: boa_ast::SourceText,
}
impl Inner {
    fn new(source_text: boa_ast::SourceText) -> Self {
        Self { source_text }
    }
}

#[derive(Default, Trace, Finalize, Clone)]
pub(crate) struct SourceText {
    #[unsafe_ignore_trace]
    source_text: Option<Rc<Inner>>,
}

impl SourceText {
    #[must_use]
    pub(crate) fn new(mut source_text: boa_ast::SourceText) -> Self {
        // (OHB) Built-in scripts keep no source text at all.
        if DROP_SOURCE.with(std::cell::Cell::get) {
            return Self::new_empty();
        }
        source_text.shrink_to_fit();
        Self {
            source_text: Some(Rc::new(Inner::new(source_text))),
        }
    }

    fn new_empty() -> Self {
        Self { source_text: None }
    }

    #[inline]
    fn inner(&self) -> Option<&boa_ast::SourceText> {
        self.source_text.as_ref().map(|x| &x.source_text)
    }

    fn is_empty(&self) -> bool {
        self.source_text.is_none()
    }
}

/// Contains pointer to source code and span of the object.
#[derive(Default, Clone)]
pub struct SpannedSourceText {
    source_text: SourceText,
    span: Option<LinearSpan>,
}

impl SpannedSourceText {
    pub(crate) fn new(source_text: SourceText, span: Option<LinearSpan>) -> Self {
        Self { source_text, span }
    }

    pub(crate) fn new_source_only(source_text: SourceText) -> Self {
        Self {
            source_text,
            span: None,
        }
    }

    pub(crate) fn new_empty() -> Self {
        Self {
            source_text: SourceText::new_empty(),
            span: None,
        }
    }

    /// Creates new [`SpannedSourceText`] with the same [`SourceText`] but without its span.
    pub(crate) fn clone_only_source(&self) -> Self {
        Self {
            source_text: self.source_text.clone(),
            span: None,
        }
    }

    /// Returns the [`SourceText`].
    #[must_use]
    pub(crate) fn source_text(&self) -> SourceText {
        self.source_text.clone()
    }

    /// Test if the span is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let span_is_empty = if let Some(x) = self.span {
            x.is_empty()
        } else {
            true
        };
        span_is_empty || self.source_text.is_empty()
    }

    /// OHB patch: a copy holding only this span's text (or nothing when there
    /// is no span), for code compiled from `eval`: a small closure that
    /// outlives an eval'd script must not keep the whole script's text
    /// alive (anti-bot scripts eval a fresh 200K-character program many
    /// times a second).
    #[must_use]
    pub(crate) fn compacted(&self) -> Self {
        let (Some(src), Some(span)) = (self.source_text.inner(), self.span) else {
            return Self::new_empty();
        };
        let whole = src.cur_linear_position().pos();
        let len = span.end().pos() - span.start().pos();
        if whole < 16 * 1024 || len * 4 > whole {
            return self.clone();
        }
        let mut copy = boa_ast::SourceText::with_capacity(len);
        for &cu in src.get_code_points_from_span(span) {
            copy.collect_code_point(u32::from(cu));
        }
        let new_span = LinearSpan::new(boa_ast::LinearPosition::new(0), boa_ast::LinearPosition::new(len));
        Self { source_text: SourceText::new(copy), span: Some(new_span) }
    }

    /// Gets inner code points.
    #[must_use]
    pub fn to_code_points(&self) -> Option<&[u16]> {
        if let (Some(source_text), Some(span)) = (self.source_text.inner(), self.span) {
            Some(source_text.get_code_points_from_span(span))
        } else {
            None
        }
    }
}

impl std::fmt::Debug for SpannedSourceText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceTextSpanned").finish()
    }
}
impl std::fmt::Debug for SourceText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceTextInner").finish()
    }
}

thread_local! {
    /// Debug stats of compiled code blocks: count, bytecode, constants, ic, bindings, source map, handlers (bytes).
    pub(crate) static CB_STATS: std::cell::Cell<[usize; 7]> = const { std::cell::Cell::new([0; 7]) };
}

/// Debug stats of compiled code blocks on this thread.
#[must_use]
pub fn code_block_stats() -> [usize; 7] {
    CB_STATS.with(std::cell::Cell::get)
}
