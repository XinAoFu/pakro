# Security Policy

Pakro manages development tools, environment variables, tasks, and system setup. Review project
configuration and third-party tool sources before trusting or executing them.

## Release identity

This checkout has no configured pakro release host, signing identity, or dedicated security email.
Automatic publishing is disabled unless a maintainer configures pakro's own destinations and
explicitly enables `PAKRO_ENABLE_RELEASE`. Upstream public keys retained in test fixtures verify
historical test data; they do not establish a pakro publisher or security contact.

Dependencies, registries, plugins, and tool download mirrors are independent upstream services.
Their existing addresses must not be presented as pakro-owned infrastructure.

## Native Security Verification

pakro provides **native Rust implementation** for security verification of tools, eliminating the need for external dependencies like `cosign`, `slsa-verifier`, `minisign`, or `gh` CLI tools. This applies to tools using the aqua backend.

### Supported Verification Methods

- **Cosign signatures**: Keyless and key-based signature verification
- **SLSA provenance**: Verification of Supply-chain Levels for Software Artifacts (SLSA) attestations
- **GitHub Artifact Attestations**: Verification of GitHub's artifact attestation system
- **Minisign verification**: Verification of minisign signatures
- **Checksum verification**: Always enabled for supported backends

### Configuration

All verification methods are enabled by default and can be configured via environment variables:

```bash
# Enable/disable specific verification methods
export PAKRO_AQUA_COSIGN=true                 # Default: true
export PAKRO_AQUA_SLSA=true                   # Default: true
export PAKRO_AQUA_GITHUB_ATTESTATIONS=true    # Default: true
export PAKRO_AQUA_MINISIGN=true               # Default: true
```

### How it Works

You will see this verification happen automatically when aqua tools are installed. The verification status is displayed during installation with progress indicators. If any verification fails, the installation will be aborted.

See the [aqua docs](https://aquaproj.github.io/docs/reference/security/cosign-slsa) for more on how verification is configured in the [aqua registry](https://github.com/aquaproj/aqua-registry).

If you notice a tool offers security verification methods (gpg/slsa/cosign/minisign/etc), consider making a PR to the aqua registry to enable verification for that tool.

## `pakro.lock`

pakro has support for lockfiles which will
store/verify the checksum of tool tarballs. Committing this into your repository is a good way to ensure
that the exact same version of a tool is installed across all developers and CI/CD systems.

Not all backends support this—notably asdf plugins do not.

## asdf plugins

asdf plugins in asdf (but not with pakro's default tools) are dangerous. They are typically owned by random developers
unconnected to either asdf or the tool vendor. They may get hacked or maliciously inject code into
their plugin that could trivially execute code on your machine.

asdf plugins are not used for tools inside the bundled registry whenever possible.
Sometimes it is not possible to use more secure backends like aqua/ubi because tools have complex install
setups or need to export env vars. Some entries depend on third-party plugins hosted in the
[mise-plugins organization](https://github.com/mise-plugins). These repositories retain their real
upstream names and are independent of pakro's release identity.

Of course if you _manually_ add plugins not from the mise-plugins org you will want to ensure they
are coming from a trusted source.

Please contribute to this effort to migrate away from asdf plugins by checking if a tool works in ubi or aqua and submitting a PR to
the bundled registry to add it. If it doesn't work
in ubi or is missing from aqua, submit an issue or PR to the respective project to add it. New tools
using asdf are **not** likely to be accepted unless they cannot be supported with any other backend.

## Reporting a Vulnerability

If the repository hosting this checkout enables private vulnerability reporting, use its Security
tab. Otherwise use a private contact channel explicitly provided by its maintainer. No security
email address or encryption key is claimed here until the pakro maintainer supplies one.
