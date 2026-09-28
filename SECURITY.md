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
- **Home and root are never deleted**: The home directory, `/` and any ancestor of home are refused outright
- **Agent configuration is protected**: `~/.claude.json`, `~/.claude/settings.json`, memory, skills, agents, commands, plugins and `history.jsonl`, and Codex `auth.json`, `config.toml` and memories; a directory containing `.claude.json`, `auth.json`, `CLAUDE.md` or `AGENTS.md` is never deleted wholesale
- **Git operations are re-verified at execution time**: A worktree is removed only if it is still clean when the action runs, `--force` is never used, and branches are deleted with `git branch -d`, never `-D`
- **Duplicate files are verified before replacement**: Contents are re-hashed immediately before a duplicate is replaced by a clone or hardlink, and a mismatch aborts
- **Agent access is limited**: Over MCP, applying a plan requires explicit confirmation and Danger actions are never offered; plans expire after an hour. Guard auto-clean is opt-in and runs Safe actions only
- **Local only**: Void does not upload scan results or any other data
