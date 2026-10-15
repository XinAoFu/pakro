# pakro-bootstrap

Resource identities, secret-safe plan output, and dependency ordering for
`pakro bootstrap`. The pakro application gathers resource state and supplies it
to this crate; this crate validates the graph and produces the ordered plan.

The code is split by responsibility: `state` defines shared desired states,
`resource` defines serializable resource data, `plan` validates and orders the
graph, and `policy` adds bootstrap-specific dependency rules.

This is an internal component of pakro; its API may change between releases.
