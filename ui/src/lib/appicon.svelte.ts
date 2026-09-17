// The app icon the user picked (#173). Rust owns the OS-facing surfaces (window, taskbar, tray);
// this is only the copy the SPA draws in the titlebar, so the two never disagree.

import { convertFileSrc } from '@tauri-apps/api/core';
import { appIconPath, setAppIcon } from './api';
import fallback from '$lib/assets/favicon.svg';

/** `src` is always drawable; `custom` says whether it is the user's file or the bundled mark.
 *  The titlebar draws the theme-aware <Logo> unless there is a custom icon to honour. */
export const appIcon = $state({ src: fallback, custom: false });

/**
 * Re-read the icon from Rust. The cache-buster matters: replacing the icon keeps the same path,
 * and the webview would otherwise redraw the image it already has.
 */
export async function loadAppIcon(): Promise<void> {
	const path = await appIconPath();
	appIcon.src = path ? `${convertFileSrc(path)}?v=${Date.now()}` : fallback;
	appIcon.custom = !!path;
}

export async function chooseAppIcon(path: string | null): Promise<void> {
	await setAppIcon(path);
	await loadAppIcon();
}
