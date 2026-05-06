//! Test-only helpers for env-var manipulation.
//!
//! Edition 2024 made `std::env::set_var` and `remove_var` `unsafe` because
//! they can race with reads from other threads. Every caller of these
//! helpers must be `#[serial]` (via `serial_test`), which serializes the
//! tests that touch env state and makes the mutation race-free in practice.

#![cfg(test)]

pub fn set_env(key: &str, val: &str) {
    // SAFETY: callers are `#[serial]`; no concurrent env access can race
    // with this mutation.
    unsafe { std::env::set_var(key, val) };
}

pub fn remove_env(key: &str) {
    // SAFETY: callers are `#[serial]`.
    unsafe { std::env::remove_var(key) };
}

pub fn remove_envs(keys: &[&str]) {
    for key in keys {
        remove_env(key);
    }
}
