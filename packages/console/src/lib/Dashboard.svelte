<script lang='ts'>
	import {
		badge,
		badgeVariant,
		button,
		buttonPrimary,
		detailCell,
		errorText,
		heading,
		logList,
		metaGrid,
		muted,
		okText,
		panel,
		row,
		rowActive,
		table,
		warnText,
	} from '../app.css'
	import {
		ApiError,
		type BackupEntry,
		type FleetStatus,
		getBackups,
		getReport,
		type LastResult,
		postBackup,
		postPrune,
		type ReportEntry,
		type SlugStatus,
		UnauthorizedError,
	} from './api'

	let { status, runningSlug, onunauthorized }: {
		status: FleetStatus
		runningSlug: string | null
		onunauthorized: () => void
	} = $props()

	let selectedSlug = $state<string | null>(null)
	let busy = $state<'backup' | 'prune' | 'report' | null>(null)
	let jobMessage = $state<{ kind: 'ok' | 'error'; text: string } | null>(null)
	let pruneMessage = $state<string | null>(null)
	let backups = $state<BackupEntry[] | null>(null)
	let backupsError = $state<string | null>(null)
	let report = $state<ReportEntry[] | null>(null)
	let reportError = $state<string | null>(null)

	const selected = $derived(
		status.slugs.find((s) => s.slug === selectedSlug) ?? null,
	)

	const levelVariant = ['never', 'ok', 'skipped', 'error'] as const
	const levelName = ['debug', 'info', 'warn', 'error'] as const

	function resultVariant(result: LastResult) {
		return result === 'ok' || result === 'skipped' || result === 'error'
			? result
			: 'never'
	}

	function pad(n: number): string {
		return String(n).padStart(2, '0')
	}

	function formatMs(ms: number | null | undefined): string {
		if (ms == null) return '—'
		const d = new Date(ms)
		return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${
			pad(d.getHours())
		}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
	}

	function formatBytes(bytes: number): string {
		if (!Number.isFinite(bytes) || bytes < 0) return '—'
		const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
		let value = bytes
		let unit = 0
		while (value >= 1024 && unit < units.length - 1) {
			value /= 1024
			unit += 1
		}
		return `${
			unit === 0 || value >= 100 ? Math.round(value) : value.toFixed(1)
		} ${units[unit]}`
	}

	function text(err: unknown): string {
		return err instanceof Error ? err.message : String(err)
	}

	function authFailure(err: unknown): boolean {
		if (err instanceof UnauthorizedError) {
			onunauthorized()
			return true
		}
		return false
	}

	async function loadBackups(slug: string) {
		backupsError = null
		try {
			backups = (await getBackups(slug)).backups
		} catch (err) {
			if (!authFailure(err)) backupsError = text(err)
		}
	}

	function select(slug: string) {
		selectedSlug = selectedSlug === slug ? null : slug
		jobMessage = null
		pruneMessage = null
		backups = null
		backupsError = null
		report = null
		reportError = null
		if (selectedSlug !== null) void loadBackups(selectedSlug)
	}

	async function backupNow() {
		if (!selected) return
		busy = 'backup'
		jobMessage = null
		try {
			const res = await postBackup(selected.slug)
			jobMessage = {
				kind: res.state.last_result === 'error' ? 'error' : 'ok',
				text: res.state.detail ?? `backup finished (${res.state.last_result})`,
			}
			void loadBackups(selected.slug)
		} catch (err) {
			if (!authFailure(err)) jobMessage = { kind: 'error', text: text(err) }
		} finally {
			busy = null
		}
	}

	async function pruneNow() {
		if (!selected) return
		busy = 'prune'
		pruneMessage = null
		try {
			const res = await postPrune(selected.slug)
			pruneMessage = `pruned: kept ${res.kept}, removed ${res.removed}`
			void loadBackups(selected.slug)
		} catch (err) {
			if (!authFailure(err)) jobMessage = { kind: 'error', text: text(err) }
		} finally {
			busy = null
		}
	}

	async function toggleReport() {
		if (report !== null || reportError !== null) {
			report = null
			reportError = null
			return
		}
		if (!selected) return
		busy = 'report'
		reportError = null
		try {
			report = (await getReport(selected.slug, 200)).entries
		} catch (err) {
			if (authFailure(err)) {
				reportError = 'signed out'
			} else if (err instanceof ApiError && err.status === 409) {
				reportError = err.hint ?? 'database in use (owner running?)'
			} else {
				reportError = text(err)
			}
		} finally {
			busy = null
		}
	}
</script>

<div class={panel}>
	<table class={table}>
		<thead>
			<tr>
				<th>slug</th>
				<th>name</th>
				<th>result</th>
				<th>last run</th>
				<th>next run</th>
				<th>backups</th>
				<th>detail</th>
			</tr>
		</thead>
		<tbody>
			{#each status.slugs as s (s.slug)}
				<tr
					class={selectedSlug === s.slug ? rowActive : undefined}
					onclick={() => select(s.slug)}
				>
					<td>{s.slug} {#if runningSlug === s.slug}<span class={`${badge} ${badgeVariant.running}`}>running…</span>{/if}</td>
					<td>{s.name}</td>
					<td>
						<span
							class={`${badge} ${badgeVariant[resultVariant(s.last_result)]}`}
						>{s.last_result}</span>
					</td>
					<td>{formatMs(s.last_run_ms)}</td>
					<td>{formatMs(s.next_run_ms)}</td>
					<td>{s.backups}</td>
					<td class={detailCell}>{s.detail ?? '—'}</td>
				</tr>
			{:else}
				<tr>
					<td colspan={7} class={muted}>
						no databases configured — check fleet config
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>

{#if selected}
	<div class={panel}>
		<h2 class={heading}>
			{selected.slug} <span class={muted}>— {selected.name}</span>
		</h2>
		<dl class={metaGrid}>
			<dt>data_dir</dt>
			<dd>{selected.data_dir}</dd>
			<dt>cron</dt>
			<dd>{selected.cron}</dd>
			<dt>retention</dt>
			<dd>{selected.retention_days} days</dd>
			<dt>initialized</dt>
			<dd>{selected.initialized ? 'yes' : 'no'}</dd>
			<dt>last result</dt>
			<dd>
				<span
					class={`${badge} ${badgeVariant[resultVariant(selected.last_result)]}`}
				>{selected.last_result}</span>
			</dd>
			<dt>last run</dt>
			<dd>{formatMs(selected.last_run_ms)}</dd>
			<dt>next run</dt>
			<dd>{formatMs(selected.next_run_ms)}</dd>
			<dt>applied lsn</dt>
			<dd>{selected.applied_lsn ?? '—'}</dd>
			<dt>last backup</dt>
			<dd>{selected.last_backup ?? '—'}</dd>
		</dl>
		{#if selected.detail}
			<p class={muted}>{selected.detail}</p>
		{/if}
		<div class={row}>
			<button
				class={`${button} ${buttonPrimary}`}
				disabled={busy !== null || !selected.initialized}
				onclick={backupNow}
			>
				{busy === 'backup' ? 'backing up…' : 'backup now'}
			</button>
			<button
				class={button}
				disabled={busy !== null || !selected.initialized}
				onclick={pruneNow}
			>
				{busy === 'prune' ? 'pruning…' : 'prune'}
			</button>
			<button
				class={button}
				disabled={busy === 'report'}
				onclick={toggleReport}
			>
				{
					report !== null || reportError !== null
					? 'close report'
					: 'view report (last 200)'
				}
			</button>
		</div>
		{#if jobMessage}
			<p class={jobMessage.kind === 'error' ? errorText : okText}>
				{jobMessage.text}
			</p>
		{/if}
		{#if pruneMessage}
			<p class={okText}>{pruneMessage}</p>
		{/if}
		{#if backupsError}
			<p class={errorText}>backups: {backupsError}</p>
		{/if}

		{#if backups !== null}
			{#if backups.length === 0}
				<p class={muted}>no backups yet</p>
			{:else}
				<table class={table}>
					<thead>
						<tr>
							<th>backup</th>
							<th>created</th>
							<th>size</th>
							<th>files</th>
							<th>applied lsn</th>
						</tr>
					</thead>
					<tbody>
						{#each backups as b (b.name)}
							<tr>
								<td>{b.name}</td>
								<td>{formatMs(b.ts_ms)}</td>
								<td>{formatBytes(b.bytes)}</td>
								<td>{b.files}</td>
								<td>{b.applied_lsn ?? '—'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		{/if}
	</div>
{/if}

{#if report !== null || reportError !== null}
	<div class={panel}>
		<h3 class={heading}>report — {selectedSlug}</h3>
		{#if reportError !== null}
			<p class={warnText}>{reportError}</p>
		{:else if report !== null}
			{#if report.length === 0}
				<p class={muted}>no report entries</p>
			{:else}
				<ul class={logList}>
					{#each report as entry, i (i)}
						<li>
							<span class={muted}>{formatMs(entry.ts_ms)}</span>
							<span
								class={`${badge} ${badgeVariant[levelVariant[entry.level]]}`}
							>
								{levelName[entry.level] ?? entry.level}
							</span>
							<span>{entry.event}</span>
							{#if entry.data != null}
								<span class={muted}>{JSON.stringify(entry.data)}</span>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}
		{/if}
	</div>
{/if}
