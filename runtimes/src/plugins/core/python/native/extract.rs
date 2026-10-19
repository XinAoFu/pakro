//! Extract verified native archives only into a live unpublished language owner.
mod owned;
#[cfg(test)]
pub(crate) use owned::install_test_read_hook;
pub(crate) use owned::{NativeExtractionReceipt, install_into_owner};

#[cfg(all(test, unix, feature = "self_update"))]
mod tests;
