# WindySync design

WindySync moves a Windows PC's working setup to another Windows PC. It is the Windows member of a family:

| App | Moves | Short name |
|-|-|-|
| MacySync | Mac to Mac | MYSY |
| WindySync | Windows to Windows | WYSY |
| LinySync | Linux to Linux | LYSY |
| AnySync | any of them to any of them | NYSYN |

WindySync is AnySync with one system: `look::SYSTEMS` holds only Windows, so the window lists and draws only Windows
devices and the command line refuses a device of another system. The rest of this document is AnySync's design,
which WindySync shares; where it speaks of other systems, both ends here run Windows.

## What stays the same as MacySync

- **The bus.** A private GitHub repository of the person's (`<login>/windysync-bus`) carries device records, requests,
  offers and live progress. GitHub is the control plane only; no file content goes there.
- **Sealed documents.** Offers, picks and the stream key are crypto_box sealed boxes for the other device's key,
  stored as base64 text.
- **The stream.** Files go device to device over TCP, as tar inside an XChaCha20-Poly1305 stream, on the LAN or
  through WindySync's own userspace Tailscale node (its own state dir, socket and SOCKS5 port, never touching a
  Tailscale already installed).
- **The flow.** One side offers a tree, the other picks, only the picks travel, and an AI agent finishes the setup
  from a brief (AGENTS.md and GOAL.md) on the receiving side.
- **Resuming.** A dropped connection times out in a minute and the receiver asks again for what it still needs;
  `--accept` picks up a stopped transfer.

## What changes: logical items

MacySync tars paths relative to `/` and maps the old home onto the new one. That breaks across systems:
`~/Library/Application Support/Code/User` on a Mac is `%APPDATA%\Code\User` on Windows and `~/.config/Code/User`
on Linux.

So every WindySync item has a **logical id** and, per system, the **real paths** it lives at:

```
apps/vscode/settings   mac:   ~/Library/Application Support/Code/User/{settings.json,keybindings.json,snippets}
                       win:   %APPDATA%\Code\User\{settings.json,keybindings.json,snippets}
                       linux: ~/.config/Code/User/{settings.json,keybindings.json,snippets}
```

The sender tars each item's files under `<id>/` (the logical root), never under its own paths. The receiver unpacks
into staging and places `<id>/<rest>` at its own path for that item. An item only offered when the receiver's
system has a place for it.

After placing, text configs get their paths rewritten: the old home becomes the new home, and paths change
separators when the systems differ (`C:\Users\ege\proj` becomes `/home/ege/proj`). Each rewrite is listed in the
brief with a copy of the file before it.

## What moves across systems

| Group | Items | Notes |
|-|-|-|
| Files | Desktop, Documents, Downloads, Pictures, Music, Videos, and folders of projects | Each top-level item can be picked; `node_modules`, `target`, `.venv` and the like stay behind in the person's own folders only |
| Shell and git | `.gitconfig`, `.gitignore_global`, SSH keys and config, GnuPG | SSH and GnuPG permissions are set again (700 and 600) on macOS and Linux |
| Shell config | `.zshrc`, `.bashrc`, PowerShell profile | Copied as reference files for the agent when the shells differ, placed when they match |
| Editors | VS Code, Cursor, Zed, JetBrains settings; VS Code extension list | Extensions reinstall from the list (`code --install-extension`) |
| Terminals | Wave config, widgets, workspaces and layouts; Windows Terminal settings | |
| Notes | Obsidian vault list and vaults, Opensidian | Vault paths rewritten |
| Browsers | Bookmarks, extensions list, settings of Chrome, Brave, Edge, Helium, Firefox | Saved passwords and cookies are encrypted with a key held by the system (macOS keychain, Windows DPAPI, Linux secret service) and can't be read on another system: the brief points to the browser's own sync or its password export |
| AI tools | Claude Code settings, skills, plugins, memory; Codex config | The app itself is installed fresh; logins are done again |
| Tool lists | Homebrew, winget, apt, dnf, pacman, flatpak, npm -g, cargo install, pipx | Sent as lists; the agent maps packages to the other system's manager |

What never moves across systems: app binaries (they are installed from the list instead), system settings, the
keychain or credential store as a whole, and anything tied to the machine's identity (Tailscale node, RustDesk ID).

## Devices

Each device registers with an id, a name the person can change, the system it runs and a look. The look is drawing
only, all original shapes and no company logos: MacySync's MacBook drawings for a Mac laptop (a plain monitor for a
Mac desktop), a monitor or a laptop for Windows, and a laptop or a monitor with the distro's name on a small badge for
Linux. The picker mixes the three systems' types. The device running the app is always on the left, the other on the
right; the lines run from sender to receiver.

## Plan

1. AnySync's engine and window, Windows only, with its own palette, logo and window shape.
2. The Windows build in CI, and a release with it attached.
3. Site at windysync.vercel.app.

## Risks

- `tar` exists on all three (bsdtar ships with Windows 10 and later), but its flags differ; the stream only uses
  `-c -f - -C <dir> <paths>` and `-x -f - -C <dir>`.
- Windows has no `sh`: commands that make lists (winget export) run under PowerShell there.
- Symbolic links on Windows need developer mode; they become copies, and the brief says so.
- Line endings: text configs keep theirs; only the path rewrite touches them.
