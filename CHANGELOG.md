# Changelog

## [Unreleased]

## [0.3.0] - 2026-10-20

- Browse Nikon ND2 files across channels, Z positions, time points and other dimensions.
- Show ND2 Z and time maximum-intensity projections.
- Browse Zeiss CZI files.
- Share an image-memory limit across windows, set it in Settings, and preload ND2 frames by priority.
- Move updates and appearance into a Settings view.
- Adjust each Color connection's brightness and drag the range graph lines directly.
- Show the full NeuroQP logo and Aperture name in the header.
- Remove the New window toolbar button; Ctrl+N still opens a new window.
- Keep image memory management running after a failed update install.
- Keep decoded frames cached when frame display is cancelled.
- Replace a stopped image decoder and retry once after it crashes.
- Stop false "open in another window" and "another instance is starting" warnings while Aperture starts helper processes.
- Keep preview, detail and annotation errors visible in the status bar.

## [0.2.1] - 2026-10-15

Includes the 0.2.0 candidate features for users upgrading from 0.1.1.

- Open images in separate Aperture windows, including from your file manager.
- Warn before overwriting images open elsewhere or changed on disk.
- Keep concurrent image caches and application updates coordinated.
- Fit newly opened images after the viewer's initial layout and resize complete.

## [0.2.0] - 2026-10-14

- Open images in separate Aperture windows, including from your file manager.
- Warn before overwriting images open elsewhere or changed on disk.
- Keep concurrent image caches and application updates coordinated.

## [0.1.1] - 2026-10-13

- Initial Aperture desktop viewer release.
- View ordinary images, TIFF and BigTIFF with progressive previews and tiled zoom.
- Edit ImageJ-compatible multi-point annotations without changing source pixels.
- Adjust display range and channel colors, and navigate images in the same folder.
- Windows x64 installer and portable executable, and Linux x86_64 AppImage.
- Windows 10 compatibility is unverified; desktop acceptance targets Windows 11.
- Windows publisher signing is pending; downloads may show an unknown-publisher warning.
- Fix window closing after checking for unsaved point annotations.

## [0.1.0] - 2026-10-13

- Candidate stopped before stable publication because its native Close handler could not complete.
