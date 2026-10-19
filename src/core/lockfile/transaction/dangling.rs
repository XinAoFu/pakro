//! The native-only right to replace one actually observed dangling symlink.
//! This never constructs a missing project-lock image or Python fresh slot.

use std::path::Path;
use std::time::Instant;

use eyre::Result;

pub(crate) struct PreparedNativeDanglingLock {
    #[cfg(unix)]
    pin: super::unix::AliasPin,
}
impl std::fmt::Debug for PreparedNativeDanglingLock {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("PreparedNativeDanglingLock")
            .finish_non_exhaustive()
    }
}
impl PreparedNativeDanglingLock {
    pub(crate) fn capture(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            let pin = super::unix::AliasPin::capture(path)?;
            pin.check_dangling()?;
            Ok(Self { pin })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            eyre::bail!("native dangling lock publication requires Unix");
        }
    }
    pub(crate) fn publish(self, bytes: &[u8], max_bytes: usize, deadline: Instant) -> Result<()> {
        #[cfg(unix)]
        return super::unix::publish_dangling(self.pin, bytes, max_bytes, deadline, |_| {});
        #[cfg(not(unix))]
        {
            let _ = (bytes, max_bytes, deadline);
            eyre::bail!("native dangling lock publication requires Unix");
        }
    }
    #[cfg(all(test, unix))]
    fn publish_with_hook(
        self,
        bytes: &[u8],
        max_bytes: usize,
        deadline: Instant,
        hook: impl FnOnce(&Path),
    ) -> Result<()> {
        super::unix::publish_dangling(self.pin, bytes, max_bytes, deadline, hook)
    }
}

#[cfg(all(test, unix))]
mod tests;
