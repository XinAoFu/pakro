# Security Policy

Pakro manages language runtimes, dependency packages and project selections.
Only explicit installation requests authorize obtaining missing artifacts.
Local queries do not install, repair or update project declarations.

Shared objects are checked against their recorded content and source identities.
Strict Python restoration preserves the exact lock and rejects mismatched inputs
or rebuilt output. Current acceptance is limited to the platform and artifact
cases recorded in [PLAN](../docs/PLAN.md).

This checkout has no configured Pakro release host, signing identity or security
email. It does not run an inherited publication workflow. Third-party artifact
hosts are independent services, not Pakro-owned infrastructure.

If this repository enables private vulnerability reporting, use its Security
tab. Otherwise use a private contact channel explicitly provided by its maintainer.
