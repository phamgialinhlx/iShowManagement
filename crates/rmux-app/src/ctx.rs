//! The process-wide context every command receives.

use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::host::Host;

type AnyStore = Arc<dyn Any + Send + Sync>;

/// The [`Host`] plus a type-map of long-lived stores.
///
/// **Stores are keyed by type, so adding one never means editing `Ctx`.** A
/// module that needs state (a `TerminalStore`, an `FsStore`, …) asks for it:
///
/// ```ignore
/// let store = call.ctx().store::<FsStore>();   // created on first use
/// ```
///
/// [`Ctx::store`] inserts `T::default()` the first time and hands every later
/// caller the same `Arc`. A store that cannot be `Default` (or that must be
/// shared with code outside the registry, like a desktop `tauri::State`) is
/// registered up front with [`Ctx::provide`]; asking for it afterwards returns
/// that instance.
pub struct Ctx {
    host: Arc<dyn Host>,
    stores: Mutex<HashMap<TypeId, AnyStore>>,
}

impl Ctx {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host, stores: Mutex::new(HashMap::new()) }
    }

    pub fn host(&self) -> &dyn Host {
        self.host.as_ref()
    }

    /// The host as an owned handle, for work that outlives the call.
    pub fn host_arc(&self) -> Arc<dyn Host> {
        self.host.clone()
    }

    /// The store of type `T`, created with `Default` on first use.
    pub fn store<T: Default + Send + Sync + 'static>(&self) -> Arc<T> {
        let mut stores = self.stores.lock();
        let entry = stores.entry(TypeId::of::<T>()).or_insert_with(|| Arc::new(T::default()));
        downcast(entry.clone())
    }

    /// The store of type `T` if one exists, without creating it.
    pub fn try_store<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.stores.lock().get(&TypeId::of::<T>()).cloned().map(downcast)
    }

    /// Register a store explicitly.
    ///
    /// # Panics
    ///
    /// When a store of this type already exists. Two instances of one store is
    /// two sources of truth — exactly the bug this map exists to prevent — so a
    /// second registration is a programming error, not a replacement.
    pub fn provide<T: Send + Sync + 'static>(&self, value: Arc<T>) -> Arc<T> {
        let mut stores = self.stores.lock();
        let id = TypeId::of::<T>();
        assert!(!stores.contains_key(&id), "store {} provided twice", type_name::<T>());
        stores.insert(id, value.clone());
        value
    }
}

fn downcast<T: Send + Sync + 'static>(store: AnyStore) -> Arc<T> {
    // The key is `TypeId::of::<T>()`, so the value is a `T` by construction.
    store.downcast::<T>().unwrap_or_else(|_| unreachable!("store keyed by its own TypeId"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::SecretStore;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NoHost;
    impl Host for NoHost {
        fn resource_dir(&self) -> Option<PathBuf> {
            None
        }
        fn data_dir(&self) -> anyhow::Result<PathBuf> {
            anyhow::bail!("none")
        }
        fn cache_dir(&self) -> anyhow::Result<PathBuf> {
            anyhow::bail!("none")
        }
        fn emit(&self, _: &str, _: serde_json::Value) {}
        fn secrets(&self) -> Option<&dyn SecretStore> {
            None
        }
    }

    #[derive(Default)]
    struct Counter(AtomicUsize);

    #[test]
    fn a_store_is_created_once_and_shared() {
        let ctx = Ctx::new(Arc::new(NoHost));
        assert!(ctx.try_store::<Counter>().is_none());
        ctx.store::<Counter>().0.fetch_add(1, Ordering::SeqCst);
        ctx.store::<Counter>().0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(ctx.store::<Counter>().0.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_provided_store_is_the_one_handed_out() {
        let ctx = Ctx::new(Arc::new(NoHost));
        let mine = Arc::new(Counter(AtomicUsize::new(7)));
        ctx.provide(mine.clone());
        assert!(Arc::ptr_eq(&mine, &ctx.store::<Counter>()));
    }

    #[test]
    #[should_panic(expected = "provided twice")]
    fn providing_a_store_twice_panics() {
        let ctx = Ctx::new(Arc::new(NoHost));
        ctx.provide(Arc::new(Counter::default()));
        ctx.provide(Arc::new(Counter::default()));
    }
}
