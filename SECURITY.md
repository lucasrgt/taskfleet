# Security Policy

## Supported versions

Security fixes are applied to the latest released version of Taskfleet.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository. Do
not open a public issue containing exploit details, secrets, or private task
data. Include the affected version, operating system, reproduction steps,
impact, and any proposed mitigation. A maintainer will acknowledge the report
and coordinate disclosure through the advisory.

## Trust boundary

Taskfleet reads arbitrary task content but treats `taskfleet.toml` and the Git
repository containing it as trusted code. Configured gates intentionally run
local executables with the invoking user's permissions. Task fields are passed
to those processes as JSON on standard input and are never interpolated into a
shell command by Taskfleet.

The MCP transport is local stdio. Taskfleet does not expose a network service,
manage tracker or model credentials, or sandbox configured gate commands.
