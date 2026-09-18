<script lang="ts">
	// Account control for the titlebar (context/15) — moved out of the sidebar so sign-in lives in the
	// top bar. Its own component because Titlebar.svelte already uses a single shared mx/my/menuOpen
	// for the window-control cluster; a second menu in that file would fight over them.
	// Multi-account: below the active account the menu lists the *other* saved Google accounts, one
	// click each to switch to, plus Add account (Google's AddSession flow) and per-account removal.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		UserCircleIcon,
		Logout01Icon,
		Add01Icon,
		Cancel01Icon,
		UserMinus01Icon
	} from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import * as AlertDialog from '$lib/components/ui/alert-dialog';
	import * as api from '$lib/api';
	import { auth, openChannelPicker, toast } from '$lib/player.svelte';
	import { thumb } from '$lib/thumb';
	import { anchorMenu, fitMenu, NO_ANCHOR } from '$lib/menu';
	import { t } from '$lib/i18n.svelte';

	let menuOpen = $state(false);
	let anchor = $state(NO_ANCHOR);
	let accounts = $state<api.SavedAccount[]>([]);
	let busy = $state(false);
	// The account the confirm dialog is asking about. Deliberately not cleared when the dialog
	// closes, so its name doesn't blank out mid close-animation; `confirmOpen` is the real state.
	let removing = $state<api.SavedAccount | null>(null);
	let confirmOpen = $state(false);
	// The active account already has the block above it; listing it again just repeats itself.
	const others = $derived(accounts.filter((a) => !a.active));

	/**
	 * Right-anchored under the trigger. The saved-account list
	 * is refetched on every open so it can't go stale while the menu was closed.
	 */
	async function openMenu(e: MouseEvent) {
		anchor = anchorMenu(e, { align: 'right' });
		menuOpen = !menuOpen;
		if (menuOpen) await loadAccounts();
	}

	/** Fetch the saved accounts for the menu; on failure show a toast and an empty list. */
	async function loadAccounts() {
		try {
			accounts = await api.getGoogleAccounts();
		} catch (e) {
			toast.error(String(e));
			accounts = [];
		}
	}

	// Sign-in/out state arrives via the `auth-changed` event (player.svelte.ts), which also reloads
	// the library and remounts the page — nothing to assign here.
	async function doSignOut() {
		menuOpen = false;
		await api.signOut();
	}

	function signInGoogle() {
		api.loginWebview(); // native sign-in window takes over; result arrives via auth-changed
		menuOpen = false;
	}

	/**
	 * Add a Google account. Signs the login webview out of Google first (Rust side) so the fresh
	 * sign-in lands on the account the user actually enters, which costs a password prompt.
	 */
	function addAccount() {
		api.loginWebview(true);
		menuOpen = false;
	}

	function switchChannel() {
		menuOpen = false;
		openChannelPicker();
	}

	/** Activate a saved account. `auth-changed` fires on success and the library reload follows. */
	async function chooseAccount(account: api.SavedAccount) {
		if (busy) return;
		busy = true;
		try {
			await api.switchGoogleAccount(account.id);
			menuOpen = false;
		} catch (e) {
			toast.error(String(e));
		} finally {
			busy = false;
		}
	}

	/** Ask before removing: the row's cookies are deleted and only a fresh sign-in brings them back. */
	function askRemove(account: api.SavedAccount) {
		if (busy) return;
		removing = account;
		confirmOpen = true;
	}

	/** Delete a saved account. Only inactive ones are listed, so this never signs the user out. */
	async function removeAccount() {
		const account = removing;
		if (!account) return;
		// bits-ui's Action is a plain button (only Cancel closes the dialog), so close it here.
		// Before the await, not after: the row disappears at once and a failure arrives as a toast.
		confirmOpen = false;
		busy = true;
		try {
			await api.removeGoogleAccount(account.id);
			accounts = accounts.filter((a) => a.id !== account.id);
		} catch (e) {
			toast.error(String(e));
		} finally {
			busy = false;
		}
	}
</script>

<!-- The avatar alone, as Spotify has it: the name is in the menu it opens and in the tooltip, and
     beside the search bar it only pushed the window controls around. -->
<button
	onclick={openMenu}
	title={auth.account?.signedIn ? (auth.account.name ?? t('nav.account')) : t('nav.sign_in')}
	aria-label={auth.account?.signedIn ? (auth.account.name ?? t('nav.account')) : t('nav.sign_in')}
	aria-expanded={menuOpen}
	class="group flex h-full cursor-pointer items-center px-2"
>
	<span
		class="flex size-10 items-center justify-center rounded-full bg-foreground/[0.08] transition-[background-color,scale] group-hover:scale-[1.04] group-hover:bg-foreground/[0.14] group-aria-expanded:bg-foreground/[0.14]"
	>
		{#if auth.account?.signedIn && auth.account.thumbnail}
			<!-- max-width:none defeats Tailwind Preflight's `img{max-width:100%}`, which in a tight box
			     clamps width to the content-box while height stays fixed → a vertical oval. Inline so
			     it's immune to Preflight and to stale dev CSS. -->
			<img
				src={thumb(auth.account.thumbnail, 64)}
				alt=""
				style="width:1.75rem;height:1.75rem;max-width:none"
				class="shrink-0 rounded-full object-cover"
			/>
		{:else}
			<HugeiconsIcon icon={UserCircleIcon} class="h-5 w-5 shrink-0 text-muted-foreground" />
		{/if}
	</span>
</button>

{#if menuOpen}
	<button
		class="fixed inset-0 z-40 cursor-default"
		onclick={() => (menuOpen = false)}
		aria-label={t('common.close')}
	></button>
	<div
		class="fixed z-50 min-w-56 w-72 animate-in rounded-lg glass p-1 text-popover-foreground duration-[var(--duration-quick)] ease-[var(--ease-smooth-out)] fade-in-0 zoom-in-[0.97]"
		style={anchor.style}
		{@attach fitMenu(anchor)}
	>
		{#if auth.account?.signedIn}
			<div class="px-3 pt-2 pb-2">
				<div class="truncate text-base font-bold">{auth.account.name ?? t('nav.account')}</div>
				{#if auth.account.handle || auth.account.email}
					<div class="truncate text-xs text-muted-foreground">
						{auth.account.handle ?? auth.account.email}
					</div>
				{/if}
			</div>
		{/if}

		{#if others.length > 0}
			{#each others as account (account.id)}
				<!-- One row, two targets, like PlaylistMenu's play/shuffle row: the row switches, the ✕
				     on its end removes. Siblings in a flex wrapper rather than a nested button (invalid
				     HTML), with the hover tint on the wrapper so it still reads as a single item. -->
				<div class="flex items-center rounded-[0.25rem] hover:bg-foreground/10">
					<button
						type="button"
						onclick={() => chooseAccount(account)}
						disabled={busy}
						class="menu-item w-auto min-w-0 flex-1 hover:bg-transparent disabled:cursor-wait"
					>
						{#if account.thumbnail}
							<img
								src={thumb(account.thumbnail, 48)}
								alt=""
								style="width:1.25rem;height:1.25rem;max-width:none"
								class="shrink-0 rounded-full object-cover"
							/>
						{:else}
							<HugeiconsIcon
								icon={UserCircleIcon}
								class="h-5 w-5 shrink-0 text-muted-foreground"
							/>
						{/if}
						<span class="min-w-0 flex-1">
							<span class="block truncate">{account.name ?? t('nav.account')}</span>
							{#if account.handle || account.email}
								<span class="block truncate text-xs text-muted-foreground">
									{account.handle ?? account.email}
								</span>
							{/if}
						</span>
					</button>
					<button
						type="button"
						onclick={() => askRemove(account)}
						disabled={busy}
						title={t('nav.remove_account')}
						aria-label={t('nav.remove_account')}
						class="mr-1 flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-foreground/10 hover:text-destructive disabled:cursor-wait"
					>
						<HugeiconsIcon icon={Cancel01Icon} class="h-3.5 w-3.5" />
					</button>
				</div>
			{/each}
			<div class="menu-sep"></div>
		{/if}

		{#if auth.account?.signedIn}
			<!-- Each icon sits in a box as wide as an avatar, so these labels share a column with the
			     saved-account names above instead of starting 4px short of them. -->
			<button type="button" class="menu-item" onclick={addAccount}>
				<span class="flex w-5 shrink-0 justify-center">
					<HugeiconsIcon icon={Add01Icon} class="size-4" />
				</span>
				{t('nav.add_account')}
			</button>
			<!-- Always offered, never gated on a stored "you have one channel": that answer comes from
			     a single accounts_list call at sign-in, and when it fails the switcher used to vanish
			     for good. The picker fetches the list live and shows its own error. -->
			<button type="button" class="menu-item" onclick={switchChannel}>
				<span class="flex w-5 shrink-0 justify-center">
					<HugeiconsIcon icon={UserCircleIcon} class="size-4" />
				</span>
				{t('nav.switch_channel')}
			</button>
			<button type="button" class="menu-item" onclick={doSignOut}>
				<span class="flex w-5 shrink-0 justify-center">
					<HugeiconsIcon icon={Logout01Icon} class="size-4" />
				</span>
				{t('nav.sign_out')}
			</button>
		{:else}
			<div class="px-3 pt-2 pb-2">
				{#if others.length === 0}
					<p class="text-sm font-bold">{t('nav.sign_in')}</p>
					<p class="mt-1 text-xs text-muted-foreground">
						{t('nav.sign_in_hint')}
					</p>
				{:else}
					<p class="text-xs text-muted-foreground">{t('nav.saved_accounts_hint')}</p>
				{/if}
				<!-- Signed out with saved accounts: the plain sign-in reuses the login webview's own
				     Google session, which is the fast path back into whichever account it last held.
				     Add account is the one that clears it first, so it is the way to reach a different
				     one. Both are offered rather than guessing which the user meant. The sign-in stays
				     a real button, not a menu row: signed out, it is the one thing this menu is for. -->
				<Button class="mt-3 w-full" onclick={signInGoogle}>{t('common.sign_in_google')}</Button>
			</div>
			{#if others.length > 0}
				<button type="button" class="menu-item" onclick={addAccount}>
					<span class="flex w-5 shrink-0 justify-center">
						<HugeiconsIcon icon={Add01Icon} class="size-4" />
					</span>
					{t('nav.add_account')}
				</button>
			{/if}
		{/if}
	</div>
{/if}

<!-- Outside the menu block on purpose: the menu can close under it (a switch, an auth-changed
     remount) and the confirm must not vanish with it. -->
<AlertDialog.Root bind:open={confirmOpen}>
	<AlertDialog.Content>
		<AlertDialog.Header>
			<!-- Muted, not destructive red: this deletes a row from a list, and dressing it as a
			     danger reads as "delete your Google account". -->
			<AlertDialog.Media class="text-muted-foreground">
				<HugeiconsIcon icon={UserMinus01Icon} />
			</AlertDialog.Media>
			<AlertDialog.Title>{t('nav.remove_account_title')}</AlertDialog.Title>
			<AlertDialog.Description>
				{t('nav.remove_account_desc', { name: removing?.name ?? t('nav.account') })}
			</AlertDialog.Description>
		</AlertDialog.Header>
		<AlertDialog.Footer>
			<AlertDialog.Cancel>{t('common.cancel')}</AlertDialog.Cancel>
			<AlertDialog.Action onclick={removeAccount}>
				{t('common.remove')}
			</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
