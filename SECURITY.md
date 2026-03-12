# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | Yes       |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Do NOT** open a public GitHub issue
2. Use [GitHub's private vulnerability reporting](https://github.com/eladbash/void/security/advisories/new) or reach out via GitHub issues with a general heads-up
3. Include steps to reproduce
4. Allow reasonable time for a fix before disclosure

We aim to respond within 48 hours and provide a fix within 7 days for critical issues.

## Security Considerations

Void operates on the filesystem and can delete files. The following safety measures are built in:

- **Blocked paths**: Critical directories (`.ssh`, `.gnupg`, `.aws`, `Documents`, `Desktop`, etc.) are never touched
- **Sentinel detection**: Directories containing `.env`, `credentials`, `secrets.yaml`, or key files are automatically blocked
- **Risk levels**: Every action is classified as Safe, Caution, or Danger
- **Symlink awareness**: Symlinked paths are automatically escalated to Caution risk
- **User-configurable blocklist**: Additional paths can be protected via configuration
