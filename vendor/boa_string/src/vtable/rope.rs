//! OHB patch: rope strings.
//!
//! `a + b` for long strings makes a rope node instead of copying both sides,
//! so building a string with repeated `+=` is linear rather than quadratic.
//! The rope is flattened (once, iteratively) the first time its contents are
//! needed; the flat copy is cached and the children are released.
use crate::iter::CodePointsIter;
use crate::vtable::JsStringVTable;
use crate::{JsStr, JsString, JsStringKind};
use std::cell::{Cell, OnceCell};
use std::mem::ManuallyDrop;
use std::process::abort;
use std::ptr::NonNull;

/// Concatenations shorter than this are copied eagerly.
pub(crate) const ROPE_MIN_LEN: usize = 256;

#[repr(C)]
pub(crate) struct RopeString {
    /// Embedded `VTable` - must be the first field for vtable dispatch.
    vtable: JsStringVTable,
    /// The two halves, until flattened.
    parts: Cell<Option<Box<(JsString, JsString)>>>,
    /// The flattened contents (a sequence string), once needed.
    flat: OnceCell<JsString>,
    refcount: Cell<usize>,
}

impl RopeString {
    pub(crate) fn new(left: JsString, right: JsString) -> Self {
        let len = left.len() + right.len();
        RopeString {
            vtable: JsStringVTable {
                clone: rope_clone,
                drop: rope_drop,
                as_str: rope_as_str,
                code_points: rope_code_points,
                refcount: rope_refcount,
                len,
                kind: JsStringKind::Rope,
            },
            parts: Cell::new(Some(Box::new((left, right)))),
            flat: OnceCell::new(),
            refcount: Cell::new(1),
        }
    }

    fn flatten(&self) -> &JsString {
        self.flat.get_or_init(|| {
            let Some(parts) = self.parts.take() else {
                return JsString::default();
            };
            // Collect the leaves left to right without recursion (ropes built by
            // `s += x` are deeply left-leaning).
            let mut leaves: Vec<JsStr<'_>> = Vec::new();
            let mut stack: Vec<&JsString> = vec![&parts.1, &parts.0];
            // SAFETY of the borrows below: every string pushed is owned by
            // `parts` or by a rope reachable from it, all alive until the end
            // of this closure.
            while let Some(s) = stack.pop() {
                if s.kind() == JsStringKind::Rope {
                    // SAFETY: kind checked.
                    let r: &RopeString = unsafe { s.as_inner() };
                    if let Some(f) = r.flat.get() {
                        leaves.push(f.as_str());
                        continue;
                    }
                    // SAFETY: the parts are only taken by `flatten` (of that rope,
                    // which isn't running) or on its drop (it is alive).
                    let p = unsafe { &*r.parts.as_ptr() };
                    if let Some(p) = p.as_deref() {
                        stack.push(&p.1);
                        stack.push(&p.0);
                    }
                    continue;
                }
                leaves.push(s.as_str());
            }
            let out = JsString::concat_array(&leaves);
            drop(leaves);
            drop_parts(parts);
            out
        })
    }
}

/// Drop rope children iteratively (a long `+=` chain is deep).
fn drop_parts(parts: Box<(JsString, JsString)>) {
    let mut work: Vec<JsString> = Vec::new();
    let (a, b) = *parts;
    work.push(a);
    work.push(b);
    while let Some(s) = work.pop() {
        if s.kind() == JsStringKind::Rope && s.refcount() == Some(1) {
            // The last reference: take the rope node apart here instead of
            // letting its drop recurse.
            let s = ManuallyDrop::new(s);
            // SAFETY: kind checked; we own the only reference.
            let node = unsafe { Box::from_raw(s.ptr.cast::<RopeString>().as_ptr()) };
            if let Some(p) = node.parts.take() {
                let (a, b) = *p;
                work.push(a);
                work.push(b);
            }
            drop(node);
        } else {
            drop(s);
        }
    }
}

#[inline]
fn rope_clone(vtable: NonNull<JsStringVTable>) -> JsString {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    let Some(strong) = this.refcount.get().checked_add(1) else {
        abort();
    };
    this.refcount.set(strong);
    // SAFETY: validated the string outside this function.
    unsafe { JsString::from_ptr(vtable) }
}

fn rope_drop(vtable: NonNull<JsStringVTable>) {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    let Some(new) = this.refcount.get().checked_sub(1) else {
        abort();
    };
    this.refcount.set(new);
    if new != 0 {
        return;
    }
    // SAFETY: This is the last reference, so we can deallocate.
    let node = unsafe { Box::from_raw(vtable.cast::<RopeString>().as_ptr()) };
    if let Some(p) = node.parts.take() {
        drop_parts(p);
    }
    drop(node);
}

#[inline]
fn rope_as_str(vtable: NonNull<JsStringVTable>) -> JsStr<'static> {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    let s = this.flatten().as_str();
    // SAFETY: the flat string lives as long as this rope.
    unsafe { s.as_static() }
}

#[inline]
fn rope_code_points(vtable: NonNull<JsStringVTable>) -> CodePointsIter<'static> {
    CodePointsIter::new(rope_as_str(vtable))
}

#[inline]
#[allow(clippy::unnecessary_wraps)]
fn rope_refcount(vtable: NonNull<JsStringVTable>) -> Option<usize> {
    // SAFETY: This is part of the correct vtable which is validated on construction.
    let this: &RopeString = unsafe { vtable.cast().as_ref() };
    Some(this.refcount.get())
}
