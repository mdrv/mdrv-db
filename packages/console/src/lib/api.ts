/** Typed fetch helpers for the mdrv-db daemon REST contract (same origin). */

export type LastResult = 'ok' | 'skipped' | 'error' | 'not_initialized' | 'never'

export interface SlugStatus {
	slug: string
	name: string
	data_dir: string
	cron: string
	retention_days: number
	initialized: boolean
	backups: number
	last_run_ms: number | null
	last_result: LastResult
	detail: string | null
	next_run_ms: number | null
	last_backup: string | null
	applied_lsn: number | null
}

export interface FleetStatus {
	type: 'status'
	slugs: SlugStatus[]
}

export interface BackupEntry {
	name: string
	ts_ms: number
	bytes: number
	files: number
	applied_lsn: number | null
}

export interface BackupsResponse {
	type: 'backups'
	slug: string
	backups: BackupEntry[]
}

export interface ReportEntry {
	ts_ms: number
	level: 0 | 1 | 2 | 3
	event: string
	data: unknown
}

export interface ReportResponse {
	type: 'report'
	slug: string
	entries: ReportEntry[]
}

export interface JobState {
	slug: string
	last_run_ms: number | null
	last_result: string
	detail: string | null
	next_run_ms: number | null
	last_backup: string | null
	applied_lsn: number | null
}

export interface JobFinishedResponse {
	type: 'job.finished'
	state: JobState
}

export interface PruneResponse {
	type: 'prune'
	slug: string
	kept: number
	removed: number
}

export class ApiError extends Error {
	status: number
	hint: string | null

	constructor(status: number, message: string, hint: string | null = null) {
		super(message)
		this.name = 'ApiError'
		this.status = status
		this.hint = hint
	}
}

export class UnauthorizedError extends ApiError {
	constructor() {
		super(401, 'unauthorized')
		this.name = 'UnauthorizedError'
	}
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
	let response: Response
	try {
		response = await fetch(path, { credentials: 'same-origin', ...init })
	} catch (err) {
		throw new Error(`network error: ${err instanceof Error ? err.message : String(err)}`)
	}
	if (response.status === 401) throw new UnauthorizedError()
	if (!response.ok) {
		let text = `${response.status} ${response.statusText}`.trim()
		let hint: string | null = null
		try {
			const body = (await response.json()) as { error?: string; hint?: string }
			if (typeof body.error === 'string') text = body.error
			if (typeof body.hint === 'string') hint = body.hint
		} catch {
			// non-JSON error body — keep the status line as the message
		}
		throw new ApiError(response.status, text, hint)
	}
	const body = await response.text()
	try {
		return (body.length === 0 ? undefined : JSON.parse(body)) as T
	} catch {
		throw new Error('invalid JSON in daemon response')
	}
}

const json = (body: unknown): RequestInit => ({
	headers: { 'content-type': 'application/json' },
	body: JSON.stringify(body),
})

export const getStatus = (): Promise<FleetStatus> => request<FleetStatus>('/api/status')

export const getBackups = (slug: string): Promise<BackupsResponse> =>
	request<BackupsResponse>(`/api/slugs/${encodeURIComponent(slug)}/backups`)

export const getReport = (slug: string, limit: number): Promise<ReportResponse> =>
	request<ReportResponse>(`/api/slugs/${encodeURIComponent(slug)}/report?limit=${limit}`)

export const postBackup = (slug: string): Promise<JobFinishedResponse> =>
	request<JobFinishedResponse>(`/api/slugs/${encodeURIComponent(slug)}/backup`, { method: 'POST' })

export const postPrune = (slug: string): Promise<PruneResponse> =>
	request<PruneResponse>(`/api/slugs/${encodeURIComponent(slug)}/prune`, { method: 'POST' })

export const login = (token: string): Promise<unknown> =>
	request<unknown>('/login', { method: 'POST', ...json({ token }) })

export const logout = (): Promise<unknown> => request<unknown>('/logout', { method: 'POST' })
