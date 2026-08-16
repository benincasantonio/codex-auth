# codex-auth

`codex-auth` is a small, dependency-free CLI for backing up and swapping
file-based [Codex credentials](https://learn.chatgpt.com/docs/auth?surface=app).

## Install

```sh
cargo install --path .
```

## Use

Fully quit Codex clients, including Codex in the ChatGPT desktop app, before
copying credentials. Codex can refresh `auth.json` while running and overwrite
a newly selected profile.

```sh
# Save the active credentials as ~/.codex/auth-personal.json.
codex-auth backup personal

# Replace an existing named backup explicitly.
codex-auth backup personal --force

# Load ~/.codex/auth-work.json as ~/.codex/auth.json.
codex-auth swap work
```

Profile names may contain ASCII letters, digits, `_`, and `-` only.
`CODEX_HOME` is used when set and non-empty; otherwise the CLI uses
`$HOME/.codex`.

## Security

Credential files contain access tokens. The CLI copies them as opaque bytes,
creates resulting files with `0600` permissions, and never prints their
contents. It supports file-backed credentials only, not credentials stored in
an operating-system keyring.

The CLI supports macOS and Linux.
