<script lang='ts'>
	import { onMount } from 'svelte'
	import { button, errorText, heading, muted, page, topbar } from './app.css'
	import {
		type FleetStatus,
		getStatus,
		logout,
		type SlugStatus,
		UnauthorizedError,
	} from './lib/api'
	import Dashboard from './lib/Dashboard.svelte'
	import Login from './lib/Login.svelte'
	import { connectEvents, type FleetEvent } from './lib/sse'

	let status = $state<FleetStatus | null>(null)
	let unauthorized = $state(false)
	let loadError = $state<string | null>(null)
	let stopEvents: (() => void) | null = null

	function handleEvent(event: FleetEvent) {
		if (event.type === 'status') {
			status = event
			return
		}
		if (status === null) return
		const slugs = status.slugs.map((s): SlugStatus =>
			s.slug === event.slug
				? {
					...s,
					last_run_ms: event.last_run_ms,
					last_result: event.result as SlugStatus['last_result'],
					detail: event.detail,
					next_run_ms: event.next_run_ms,
					last_backup: event.last_backup,
				}
				: s
		)
		status = { ...status, slugs }
	}

	async function load(): Promise<boolean> {
		loadError = null
		try {
			status = await getStatus()
			unauthorized = false
			return true
		} catch (err) {
			if (err instanceof UnauthorizedError) {
				unauthorized = true
			} else {
				loadError = err instanceof Error ? err.message : String(err)
			}
			return false
		}
	}

	function watchEvents() {
		stopEvents ??= connectEvents(handleEvent)
	}

	function onUnauthorized() {
		stopEvents?.()
		stopEvents = null
		status = null
		unauthorized = true
	}

	async function onLoginSuccess() {
		if (await load()) watchEvents()
	}

	async function onLogout() {
		try {
			await logout()
		} catch {
			// session may already be gone; show the login view regardless
		}
		onUnauthorized()
	}
	function retry() {
		void load().then((ok) => {
			if (ok) watchEvents()
		})
	}


	onMount(() => {
		void load().then((ok) => {
			if (ok) watchEvents()
		})
		return () => {
			stopEvents?.()
			stopEvents = null
		}
	})
</script>

<div class={page}>
	{#if unauthorized}
		<Login onsuccess={onLoginSuccess} />
	{:else if status !== null}
		<header class={topbar}>
			<h1 class={heading}>mdrv-db console</h1>
			<button class={button} onclick={onLogout}>log out</button>
		</header>
		<Dashboard {status} onunauthorized={onUnauthorized} />
	{:else if loadError !== null}
		<p class={errorText}>failed to load fleet status: {loadError}</p>
		<button class={button} onclick={retry}>retry</button>
	{:else}
		<p class={muted}>connecting…</p>
	{/if}
</div>
