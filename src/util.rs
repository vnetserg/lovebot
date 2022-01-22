use std::sync::{Arc, LockResult, RwLock, RwLockReadGuard, RwLockWriteGuard};

////////////////////////////////////////////////////////////////////////////////

pub struct Writer<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> Writer<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(value)),
        }
    }

    pub fn reader(&self) -> Reader<T> {
        Reader {
            inner: self.inner.clone(),
        }
    }

    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        self.inner.write()
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct Reader<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> Reader<T> {
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        self.inner.read()
    }
}
