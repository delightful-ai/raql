//! Compile-time-visible database-attachment discipline.
//!
//! rust-analyzer's type layer reads the database from a thread-local slot
//! (`hir::attach_db`), and panics if a different database instance is
//! attached on the same thread. Some RA APIs (`world_symbols`,
//! `parallel_prime_caches`) fan out over database *clones* whose closures
//! can run on the calling thread — calling them while attached panics at
//! runtime, and only on the operator mixes that actually hit them.
//!
//! This module turns that runtime rule into types:
//!
//! - [`Snapshot`] is the unattached context. APIs that must NOT run
//!   attached take `&mut Snapshot`.
//! - [`Snapshot::attached`] enters the attached scope and hands out an
//!   [`Attached`] witness. It takes `&mut self`, so while the scope is open
//!   the borrow checker rejects any call needing `&mut Snapshot` — the
//!   unattached-only APIs are unreachable from inside, at compile time.
//! - [`Attached`] cannot escape the scope (it is only lent by reference)
//!   and cannot cross threads (`!Send`/`!Sync` — the attachment is
//!   thread-local).
//!
//! `hir::attach_db` itself must appear in this module and nowhere else in
//! the crate (enforced by the no-shadow-state source test), so every attach
//! flows through one audited choke point.

use std::marker::PhantomData;

use hir::db::HirDatabase;
use ide_db::RootDatabase;

/// One RA snapshot in the unattached state.
pub struct Snapshot<'db> {
    db: &'db RootDatabase,
}

impl<'db> Snapshot<'db> {
    pub fn new(db: &'db RootDatabase) -> Self {
        Self { db }
    }

    /// Run `op` with the database TLS-attached.
    ///
    /// Takes `&mut self` deliberately: it makes this scope and the
    /// unattached-only APIs (`&mut Snapshot` receivers) mutually exclusive
    /// by borrow check, not by discipline.
    pub fn attached<R>(&mut self, op: impl FnOnce(&Attached<'db>) -> R) -> R {
        let attached = Attached { db: self.db, _thread_local: PhantomData };
        hir::attach_db(self.db, || op(&attached))
    }

    /// The raw database, for unattached-only APIs (the symbol index seed).
    /// Callers hold `&mut Snapshot`, which proves no attach scope is open.
    pub(crate) fn unattached_db(&mut self) -> &'db RootDatabase {
        self.db
    }
}

/// Proof that the current thread has the database TLS-attached. Only
/// [`Snapshot::attached`] and [`with_attached`] construct one.
pub struct Attached<'db> {
    db: &'db RootDatabase,
    /// `*mut ()` makes this `!Send + !Sync`: the attachment is
    /// thread-local, so the witness must not travel.
    _thread_local: PhantomData<*mut ()>,
}

impl<'db> Attached<'db> {
    pub fn db(&self) -> &'db RootDatabase {
        self.db
    }
}

/// Attached scope over a concrete snapshot, for entry points that are not
/// operator dispatch (output projection).
pub(crate) fn with_attached<R>(
    db: &RootDatabase,
    op: impl for<'a> FnOnce(&Attached<'a>) -> R,
) -> R {
    let attached = Attached { db, _thread_local: PhantomData };
    hir::attach_db(db, || op(&attached))
}

/// Attached scope for Salsa-tracked query bodies, which receive
/// `&dyn HirDatabase` and may be (re-)executed from any caller's
/// verification stack, attached or not. No witness: tracked bodies work on
/// the dyn database directly and must not reach concrete-`RootDatabase`
/// APIs (which is what keeps the parallel-fanout APIs out of them).
pub(crate) fn with_attached_dyn<R>(db: &dyn HirDatabase, op: impl FnOnce() -> R) -> R {
    hir::attach_db(db, op)
}
