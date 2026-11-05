# ADR-021: WebView2 distribution: bundle the offline installer

- **Status:** Proposed
- **Date:** 2026-11-05
- **Source:** Task E01-T01 (Tauri 2 scaffold and Windows packaging spike)

## Context

Tauri renders the UI with the Microsoft Edge WebView2 runtime on Windows. Windows 11 ships with
it, but some images (LTSC, debloated, or locked-down corporate builds) remove or freeze it.
Tauri's installer can get the runtime in one of four ways: download the bootstrapper, embed the
bootstrapper, bundle the full offline installer, or use a fixed runtime version.

Shops are often installed in places with poor or no internet (PRD §1, §11), and the installer
must succeed offline (E01-T01 acceptance).

## Decision

- Use `bundle.windows.webviewInstallMode = { "type": "offlineInstaller", "silent": true }`.
- Install per user (`nsis.installMode = "currentUser"`), so no administrator rights are needed.

## Consequences

- The installer grows by roughly 130–150 MB. That is accepted for offline reliability.
- Auto-updates (E19-T06) ship the app only, not WebView2, so update downloads stay small.
- If the size becomes a problem, the alternative is two installer flavours: a small
  "online" one with the embedded bootstrapper, and an "offline" one. That would be a new ADR.
- To be verified on a clean Windows 11 VM with networking disabled. This verification is part
  of the E01-T01 acceptance, and runs on the CI artefact.
