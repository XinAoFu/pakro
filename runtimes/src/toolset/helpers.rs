use std::sync::Arc;

use crate::backend::Backend;
use crate::toolset::tool_version::ToolVersion;

pub(super) type TVTuple = (Arc<dyn Backend>, ToolVersion);
