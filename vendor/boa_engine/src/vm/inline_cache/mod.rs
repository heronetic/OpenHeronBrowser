use itertools::Itertools;
use std::{cell::Cell, fmt};

use boa_gc::GcRefCell;
use boa_macros::{Finalize, Trace};

use crate::{
    JsString,
    object::shape::{Shape, WeakShape, slot::{Slot, SlotAttributes}},
};

#[cfg(test)]
mod tests;

pub(crate) const PIC_CAPACITY: usize = 4;

/// A cached shape-to-slot mapping for a polymorphic inline cache.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct CacheEntry {
    /// A weak reference is kept to the shape to avoid the shape preventing deallocation.
    pub(crate) shape: WeakShape,
    #[unsafe_ignore_trace]
    pub(crate) slot: Slot,
    /// OHB patch: for slots found on the prototype, the prototype's shape when
    /// cached. Deleting or reconfiguring a prototype property gives the
    /// prototype a new shape (and shifts its slots), so a hit must also match it.
    pub(crate) proto_shape: Option<WeakShape>,
}

/// The current shape of `shape`'s prototype, if any.
fn proto_shape_of(shape: &Shape) -> Option<Shape> {
    let proto = shape.prototype()?;
    let b = proto.try_borrow().ok()?;
    Some(b.shape().clone())
}

/// An inline cache entry for a property access.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct InlineCache {
    /// The property that is accessed.
    pub(crate) name: JsString,

    /// Multiple cached shape-to-slot entries.
    /// OHB patch: a Vec (no allocation until the site first caches) instead
    /// of an inline array: most access sites in a realm never run, and an
    /// inline 4-entry array made every site ~180 bytes.
    pub(crate) entries: GcRefCell<Vec<CacheEntry>>,

    /// Whether this access site has seen too many shapes and should no longer be cached.
    #[unsafe_ignore_trace]
    pub(crate) megamorphic: Cell<bool>,
}

impl fmt::Display for InlineCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(name:{} entries:", self.name.display_escaped())?;

        if self.megamorphic.get() {
            return write!(f, "(megamorphic))");
        }

        let entries = self.entries.borrow();
        let entries = entries.iter().map(|e| e.shape.to_addr_usize()).format(", ");

        write!(f, "({entries:#x}))")
    }
}

impl InlineCache {
    pub(crate) fn new(name: JsString) -> Self {
        Self {
            name,
            entries: GcRefCell::new(Vec::new()),
            megamorphic: Cell::new(false),
        }
    }

    pub(crate) fn set(&self, shape: &Shape, slot: Slot) {
        if self.megamorphic.get() {
            return;
        }

        let mut entries = self.entries.borrow_mut();

        // Add a new entry if there's space.
        if entries.len() >= PIC_CAPACITY {
            // Polymorphic cache is full, transition to megamorphic.
            self.megamorphic.set(true);
            entries.clear();
            entries.shrink_to_fit();
            return;
        }
        let proto_shape = if slot.attributes.contains(SlotAttributes::PROTOTYPE) {
            match proto_shape_of(shape) {
                Some(p) => Some(WeakShape::from(&p)),
                None => return,
            }
        } else {
            None
        };
        if entries.capacity() == 0 {
            entries.reserve_exact(1);
        }
        entries.push(CacheEntry { shape: shape.into(), slot, proto_shape });
    }

    /// Returns the cached `(Shape, Slot)` if a matching shape exists in the inline cache.
    ///
    /// Opportunistically cleans up stale weak shape references during lookup.
    pub(crate) fn get(&self, shape: &Shape) -> Option<(Shape, Slot)> {
        if self.megamorphic.get() {
            return None;
        }

        let mut entries = self.entries.borrow_mut();
        let mut i = 0;
        let mut result = None;
        let shape_addr = shape.to_addr_usize();

        // OHB: compare addresses without upgrading (no ref-count traffic);
        // a collected shape reads as address 0 and is dropped.
        while i < entries.len() {
            let addr = entries[i].shape.to_addr_usize();
            if addr == 0 {
                entries.swap_remove(i);
                continue;
            }
            if addr == shape_addr {
                if let Some(ps) = &entries[i].proto_shape {
                    let cur = shape.prototype().and_then(|p| p.try_borrow().ok().map(|b| b.shape().to_addr_usize()));
                    let cached = ps.to_addr_usize();
                    if cur.is_none() || cur != Some(cached) || cached == 0 {
                        // Stale: the prototype changed shape since caching.
                        entries.swap_remove(i);
                        break;
                    }
                }
                result = Some((shape.clone(), entries[i].slot));
                break;
            }
            i += 1;
        }

        result
    }
}
