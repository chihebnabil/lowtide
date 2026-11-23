<p align="center">
  <img src="crates/gui/icons/128x128@2x.png" width="96" alt="Lowtide" />
</p>

<h1 align="center">Lowtide</h1>

<p align="center"><b>See what your dev tools left behind, and wash it away.</b><br>
A free, open-source disk cleaner for developers on Windows.</p>

---

Every `npm install`, `cargo build`, `pip install`, Playwright download and Hugging Face model
leaves gigabytes behind, and none of those tools ever clean up after themselves. Lowtide finds it all,
tells you in plain words **what each thing is and what happens if you delete it**, and removes only
what you pick.

Built for developers, including the new wave of AI-assisted builders who never had a reason to learn
what `%LOCALAPPDATA%\uv\cache` is.

## Features

- **Overview at a glance.** See how much of your drive is junk, and clean everything safe in one click.
- **Desktop app and terminal UI.** Same engine, pick your style.
- **50+ cache locations:** npm, pnpm, Yarn, Bun, Deno, pip, uv, Poetry, Cargo, Go, NuGet, Gradle, Maven,
  Playwright, Cypress, Puppeteer, Hugging Face, PyTorch, VS Code, Cursor, Windsurf, JetBrains,
  Chrome, Edge, Brave, Firefox, Discord, Slack, Windows Temp, Windows Update, crash dumps, Recycle Bin…
- **Project build folders:** `node_modules`, `target`, `.next`, `.nuxt`, `.svelte-kit`, `.turbo`, venvs,
  `.gradle`, .NET `bin/obj`, `.terraform` and more, with how long ago each project was last built.
- **Every item explained** with a risk level:
  - **Safe**: regenerates on its own
  - **Re-download**: comes back, after a download or rebuild
  - **Review**: permanent, look first
- **Recycle Bin mode** for project folders, so you can change your mind.

## Safety

- Only known cache folders are touched. Never settings, logins, cookies, extensions, chat history or source code.
- A build folder only counts next to its project file: `target` needs `Cargo.toml`, `node_modules` needs `package.json`.
- Symlinks and junctions are removed as links, never followed (pnpm and OneDrive safe).
- Files in use are skipped, not fatal. Temp files newer than 24h are left alone.
- Every path passes a guard, so a broken `%TEMP%=C:\` can't wipe your drive.
- No account, no telemetry, no network access.

## Install

Download the installer from [Releases](../../releases), or build it yourself (see below).

## Command line

```
lowtide                          # terminal UI
lowtide scan                     # what's using space (read-only)
lowtide clean                    # clean the "safe" caches, asks first
lowtide clean --all              # also caches that need a re-download later
lowtide clean --only npm,uv      # specific targets (ids from `lowtide scan`)
lowtide projects D:\code --older-than 30            # stale build folders
lowtide projects D:\code --older-than 30 --clean --recycle
```

Run from an elevated terminal (or click **Restart as admin** in the app) to include system locations.

## Lowtide Pro (coming soon)

- **Docker & WSL:** compact the `.vhdx` disks that grow but never shrink. The free app already shows how big yours are.
- **Autopilot:** weekly cleaning, low-disk alerts, rules for idle projects, and space history.

Everything above stays free. [Join the waitlist](https://forms.gle/B9AavYZjFwtehyRF8) to hear when Pro launches.

## Build

Requires Rust and the MSVC build tools.

```
cargo build --release -p lowtide           # CLI: target\release\lowtide.exe
cargo test

cd crates/gui && npm install && npm run build   # desktop app + installer
```

### MSIX (Microsoft Store)

The release workflow also packages the app as MSIX (`crates/gui/msix/`) and uploads it as the
`msix` workflow artifact. Run it manually from the Actions tab to build without releasing.

- `Lowtide-<version>-x64.msix`: unsigned, upload this to Partner Center (the Store signs it).
  It carries the Store identity of *Lowtide: Disk Cleaner for Developers* (defaults in
  `build-msix.ps1`, overridable with the `MSIX_*` repository variables).
- `Lowtide-<version>-x64-test.msix` + `Lowtide-test.cer`: self-signed, for testing on your own PC.
  It has the same identity as the Store app, so uninstall it before installing from the Store.
  From an elevated PowerShell:

```
Import-Certificate -FilePath Lowtide-test.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage Lowtide-<version>-x64-test.msix
```

The Store build asks only for `runFullTrust` and never elevates: "Restart as admin" is hidden and the
few system locations (Windows Temp, Windows Update downloads, Delivery Optimization cache) are skipped.
Use the installer from Releases, or `lowtide` in an elevated terminal, to clean those.

Build locally with `./crates/gui/msix/build-msix.ps1 -Exe target/release/lowtide-app.exe -Version 0.2.0 -TestSign`
(needs the Windows SDK).

## License

MIT
