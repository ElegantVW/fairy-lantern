# Security policy — fairy-lantern

## Supported versions

| Tag | Status |
|-----|--------|
| latest `main` + pinned tags (see CHANGELOG.md) | supported during the public week |

No bounty program yet. This is personal/small-team software, not audited product.

## Reporting

Email: evenweaker@disroot.org with subject `[SECURITY] fairy-lantern`.
Include: version/tag, steps to reproduce, impact, logs with `FAE_DEBUG=1` if relevant.

Promise: acknowledge within 7 days, fix + credit (or anonymous if you prefer).

## Rules for reports

- Test on your own machines. Do not probe the office box or anyone else's host.
- Never include passwords, keys, mail bodies, client names, NIFs, or ROMs.
- Bulwark/Seal: unprivileged sandbox first; no destructive firewall/iptables flushes on shared nets.
