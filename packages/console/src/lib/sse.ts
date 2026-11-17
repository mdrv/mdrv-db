import type { FleetStatus } from './api'

/** Frames pushed by GET /api/events. */
export type FleetEvent =
	| FleetStatus
	| {
		type: 'meta'
		slug: string
		title: string | null
		description: string | null
		labels: string[] | null
	}
	| {
		type: 'job.started'
		slug: string
		ts_ms: number
	}
	| {
		type: 'job.finished'
		slug: string
		result: string
		detail: string | null
		last_run_ms: number | null
		next_run_ms: number | null
		last_backup: string | null
	}

/**
 * Subscribe to the daemon's SSE stream. Each frame's `data` is one JSON
 * object (see FleetEvent); comment pings are ignored by EventSource, which
 * also reconnects on its own. Returns an unsubscribe function.
 *
 * A 401 (session expired, e.g. daemon restart with an in-memory session
 * store) kills the EventSource permanently and never auto-retries, so
 * `onerror` probes the API: if the probe comes back 401 the caller's
 * `onUnauthorized` runs; otherwise the EventSource keeps its own retry.
 */
export function connectEvents(
	onEvent: (event: FleetEvent) => void,
	onUnauthorized: () => void = () => {}
): () => void {
	const source = new EventSource('/api/events')
	let probing = false
	source.onmessage = (message) => {
		try {
			onEvent(JSON.parse(message.data) as FleetEvent)
		} catch {
			// malformed frame — drop it, the stream continues
		}
	}
	source.onerror = () => {
		if (probing) return
		probing = true
		fetch('/api/status', { credentials: 'same-origin' })
			.then((res) => {
				if (res.status === 401) onUnauthorized()
			})
			.catch(() => {
				// network blip — EventSource retries on its own
			})
			.finally(() => {
				probing = false
			})
	}
	return () => source.close()
}
