<script lang='ts'>
	import {
		badge,
		button,
		detailCell,
		errorText,
		filterRow,
		heading,
		input,
		label,
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
		patchMeta,
		postBackup,
		postPrune,
		type ReportEntry,
		reorderFleet,
		type SlugStatus,
		UnauthorizedError,
	} from './api'

	let { status, runningSlug, onunauthorized }: {
		status: FleetStatus
		runningSlug: string | null
		onunauthorized: () => void
	} = $props()

	let selectedSlug = $state<string | null>(null)
	let busy = $state<'backup' | 'prune' | 'report' | 'move' | null>(null)
	let jobMessage = $state<{ kind: 'ok' | 'error'; text: string } | null>(null)
	let pruneMessage = $state<string | null>(null)
	let backups = $state<BackupEntry[] | null>(null)
	let backupsError = $state<string | null>(null)
	let report = $state<ReportEntry[] | null>(null)
	let reportError = $state<string | null>(null)

	let filter = $state('')
	let activeLabel = $state<string | null>(null)
	let editTitle = $state('')
	let editDescription = $state('')
	let editLabels = $state('')
	let metaBusy = $state(false)
	let metaMessage = $state<{ kind: 'ok' | 'error'; text: string } | null>(null)

	const selected = $derived(
		status.slugs.find((s) => s.slug === selectedSlug) ?? null,
	)

	const filtered = $derived.by(() => {
		const q = filter.trim().toLowerCase()
		return status.slugs.filter((s) => {
			if (activeLabel !== null && !(s.labels ?? []).includes(activeLabel)) return false
			if (q === '') return true
			const hay = [s.slug, s.name ?? '', s.title ?? '', ...(s.labels ?? [])]
				.join(' ')
				.toLowerCase()
			return hay.includes(q)
		})
	})

	const allLabels = $derived(
		[...new Set(status.slugs.flatMap((s) => s.labels ?? []))].sort(),
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
		const cur = status.slugs.find((s) => s.slug === selectedSlug)
		editTitle = cur?.title ?? ''
		editDescription = cur?.description ?? ''
		editLabels = (cur?.labels ?? []).join(', ')
		metaMessage = null
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

	async function move(slug: string, direction: 'up' | 'down') {
		busy = 'move'
		try {
			await reorderFleet(slug, direction)
		} catch (err) {
			if (!authFailure(err)) jobMessage = { kind: 'error', text: text(err) }
		} finally {
			busy = null
		}
	}

	async function saveMeta() {
		if (!selected) return
		metaBusy = true
		metaMessage = null
		const t = editTitle.trim()
		const d = editDescription.trim()
		const labels = editLabels.split(',').map((x) => x.trim()).filter(Boolean)
		try {
			await patchMeta(selected.slug, {
				title: t === '' ? null : t,
				description: d === '' ? null : d,
				labels: labels.length === 0 ? null : labels,
			})
			metaMessage = { kind: 'ok', text: 'saved to fleet config' }
		} catch (err) {
			if (!authFailure(err)) metaMessage = { kind: 'error', text: text(err) }
		} finally {
			metaBusy = false
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
	<div class={filterRow}>
		<input
			class={input}
			placeholder='filter by slug, title, label'
			title='filters the fleet table; labels are clickable below'
			bind:value={filter}
		/>
		{#each allLabels as lb (lb)}
			<button
				class={button}
				data-active={activeLabel === lb}
				title='show only databases with this label'
				onclick={() => (activeLabel = activeLabel === lb ? null : lb)}
			>
				{lb}
			</button>
		{/each}
	</div>
	<table class={table}>
		<thead>
			<tr>
				<th>slug</th>
				<th>name</th>
				<th>result</th>
				<th>last run</th>
				<th>next run</th>
				<th>backups</th>
				<th title='fleet display order; disabled while a filter is active'>order</th>
				<th>detail</th>
			</tr>
		</thead>
		<tbody>
			{#each filtered as s, i (s.slug)}
				<tr
					class={selectedSlug === s.slug ? rowActive : undefined}
					onclick={() => select(s.slug)}
				>
					<td>
						{s.slug} {#if runningSlug === s.slug}<span class={badge} data-tone='running'>running…</span>{/if}
						{#each s.labels ?? [] as lb (lb)}<span class={badge} data-tone='label'>{lb}</span>{/each}
					</td>
					<td>
						{#if s.title}{s.title}{#if s.name} <span class={muted}>({s.name})</span>{/if}{:else}{s.name}{/if}
					</td>
					<td>
						<span class={badge} data-tone={resultVariant(s.last_result)}
						>{s.last_result}</span>
					</td>
					<td>{formatMs(s.last_run_ms)}</td>
					<td>{formatMs(s.next_run_ms)}</td>
					<td>{s.backups}</td>
					<td>
						<button
							class={button}
							title='move this database up in the fleet list'
							disabled={i === 0 || busy !== null || filter.trim() !== '' || activeLabel !== null}
							onclick={(e) => {
								e.stopPropagation()
								void move(s.slug, 'up')
							}}
						>▲</button>
						<button
							class={button}
							title='move this database down in the fleet list'
							disabled={i === filtered.length - 1 || busy !== null || filter.trim() !== '' || activeLabel !== null}
							onclick={(e) => {
								e.stopPropagation()
								void move(s.slug, 'down')
							}}
						>▼</button>
					</td>
					<td class={detailCell}>{s.detail ?? '—'}</td>
				</tr>
			{:else}
				<tr>
					<td colspan={8} class={muted}>
						{#if status.slugs.length === 0}
							no databases configured — check fleet config
						{:else}
							no databases match the filter
						{/if}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>

{#if selected}
	<div class={panel}>
		<h2 class={heading}>
			{selected.slug} <span class={muted}>— {selected.title ?? selected.name}</span>
		</h2>
		<dl class={metaGrid}>
			<dt>data_dir</dt>
			<dd>{selected.data_dir}</dd>
				{#if selected.description}
					<dt title='from fleet config, editable below'>description</dt>
					<dd>{selected.description}</dd>
				{/if}
			<dt>cron</dt>
			<dd>{selected.cron}</dd>
			<dt>retention</dt>
			<dd>{selected.retention_days} days</dd>
			<dt>initialized</dt>
			<dd>{selected.initialized ? 'yes' : 'no'}</dd>
			<dt>last result</dt>
			<dd>
				<span class={badge} data-tone={resultVariant(selected.last_result)}
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
		<label class={label} for='meta-title'>title</label>
		<input id='meta-title' class={input} bind:value={editTitle} placeholder={selected.name ?? selected.slug} />
		<label class={label} for='meta-description'>description</label>
		<input id='meta-description' class={input} bind:value={editDescription} />
		<label class={label} for='meta-labels' title='comma-separated, max 8, each max 32 chars'>labels</label>
		<input id='meta-labels' class={input} bind:value={editLabels} title='comma-separated, max 8, each max 32 chars' placeholder='prod, weekly' />
		<div class={row}>
			<button class={button} data-variant='primary' disabled={metaBusy} onclick={saveMeta}>
				{metaBusy ? 'saving…' : 'save metadata'}
			</button>
			{#if metaMessage}<span class={metaMessage.kind === 'error' ? errorText : okText}>{metaMessage.text}</span>{/if}
		</div>
		<div class={row}>
			<button
				class={button}
				data-variant='primary'
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
								class={badge}
								data-tone={levelVariant[entry.level]}
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
