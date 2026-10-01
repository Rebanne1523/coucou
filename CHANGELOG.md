# Changelog

## Unreleased

- **Linux (CachyOS / Arch)**: Coucou now runs on Linux. The Windows port moved from `windows/` to `desktop/` and became cross-platform: layer-shell island for Wayland (KDE Plasma, Sway, Hyprland) with an X11 fallback, Unix-socket relay for Claude Code hooks, keys in the Secret Service, `PKGBUILD` and a CI workflow.
- **Supabase pill** (Windows and Linux): project and service health, Auth API latency, and Edge Function errors from the last hour. The Management API calls have not been run against a live project yet.

- Compact island on screens without a notch (#22) — thanks @Kamasoutra
