<script lang='ts'>
	import {
		button,
		buttonPrimary,
		errorText,
		heading,
		input,
		label,
		muted,
		panel,
		row,
	} from '../app.css'
	import { login, UnauthorizedError } from './api'

	let { onsuccess }: { onsuccess: () => void | Promise<void> } = $props()

	let token = $state('')
	let busy = $state(false)
	let error = $state<string | null>(null)

	async function submit(event: SubmitEvent) {
		event.preventDefault()
		if (busy || token.length === 0) return
		busy = true
		error = null
		try {
			await login(token)
			token = ''
			await onsuccess()
		} catch (err) {
			if (err instanceof UnauthorizedError) error = 'invalid token'
			else error = err instanceof Error ? err.message : String(err)
		} finally {
			busy = false
		}
	}
</script>

<form class={panel} onsubmit={submit}>
	<h1 class={heading}>mdrv-db console</h1>
	<p class={muted}>fleet dashboard — sign in with the daemon admin token</p>
	<label class={label} for='token'>admin token</label>
	<input
		id='token'
		class={input}
		type='password'
		autocomplete='current-password'
		bind:value={token}
	/>
	{#if error !== null}
		<p class={errorText}>{error}</p>
	{/if}
	<div class={row}>
		<button
			class={`${button} ${buttonPrimary}`}
			type='submit'
			disabled={busy || token.length === 0}
		>
			{busy ? 'signing in…' : 'sign in'}
		</button>
	</div>
</form>
