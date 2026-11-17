//! Locks that never poison.
//!
//! A poisoned lock means a thread panicked while holding it. Library code is
//! not allowed to panic, and every critical section here leaves its data
//! valid at each step, so a poisoned guard is recovered rather than turned
//! into a second panic.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks a mutex, whether or not it is poisoned.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Locks for reading, whether or not the lock is poisoned.
pub fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// Locks for writing, whether or not the lock is poisoned.
pub fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
