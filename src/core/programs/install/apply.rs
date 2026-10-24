//! Regression tests for the production install facade and runtime preparation.
//!
//! The former PBS-only orchestration had no production callers and duplicated
//! source selection without the runtime-pin handling in programs::python.
//! Production installation now has exactly that facade and its native adapter.

#[cfg(test)]
mod tests;
