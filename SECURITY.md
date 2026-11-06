# Security

root holds credentials for the systems it watches and a map of which parts of them are failing, so reports are taken seriously.

## Reporting a vulnerability

Please use [GitHub's private vulnerability reporting](https://github.com/aniketshukla1/root/security/advisories/new) rather than a public issue. Include what you did, what happened and which version or commit you ran.

## What the design promises

These are the properties a report would be measured against. How each works is in [the reference](docs/REFERENCE.md).

- **Credentials** stay in the OS keychain. The config file holds endpoints and options, never a secret.
- **Configuration reaches every reader already redacted.** No accessor returns an unredacted body.
- **Paths are declared, never discovered.** root opens the paths you name and does not crawl a filesystem. Reads stay read-only.
- **The assistant** is read-only by default. With actions allowed, every call stops for approval, and it has no shell tool or arbitrary-SQL tool.
- **SSH servers** are pinned to one host key on first use, and root runs fixed read-only commands with every path quoted.
- **The server** refuses to start without a token. The desktop app binds loopback only.
