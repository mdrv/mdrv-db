/**
 * @mdrv/db-events — cross-app event streams over mdrv-db.
 *
 * Three pieces, each usable standalone:
 *  1. outboxOp / OUTBOX_DDL  — producer side: emit events atomically with the
 *     data mutation (seq = 'Lsn', a free monotonic number from the envelope).
 *  2. eventsEndpoint         — GET /events?since=<seq>&topic=<t> handler with
 *     static-token auth; pollers drive everything.
 *  3. createConsumer         — poller that stores last_seq in the consumer's
 *     OWN database and advances it in the SAME envelope write as the mirrored
 *     work (crash-safe; at-least-once delivery + idempotent handlers =
 *     effectively exactly-once).
 */

export interface SqlOpBody {
	kind: 'Insert' | 'Upsert' | 'Update' | 'Delete'
	table: string
	pk_col: string
	columns: string[]
	values: unknown[]
	pk: unknown
}

export type Op = { Sql: SqlOpBody } | { BlobPut: { hash_hex: string } } | { BlobDrop: { hash_hex: string } }

/** mdrv-db MdrvDb-compatible surface: JSON-string in, JSON-string out. */
export interface EventsStore {
	execute(requestJson: string): Promise<string>
	query(sql: string, paramsJson?: string): Promise<string>
	bootstrap?(statementsJson: string): Promise<void>
}

export const OUTBOX_DDL =
	"CREATE TABLE IF NOT EXISTS outbox (seq INTEGER PRIMARY KEY, topic TEXT NOT NULL DEFAULT '', event TEXT NOT NULL, payload TEXT, created_at INTEGER NOT NULL)"

export const CURSOR_DDL =
	'CREATE TABLE IF NOT EXISTS _events_cursor (topic TEXT PRIMARY KEY, last_seq INTEGER NOT NULL)'

/** PortValue wrappers for handler-provided ops. */
export const v = {
	null: 'Null' as const,
	lsn: 'Lsn' as const,
	text: (s: string) => ({ Text: s }),
	int: (n: number) => ({ Int: Math.trunc(n) }),
	real: (f: number) => ({ Real: f }),
}

/** Wrap a JS value the way the engine expects (params + pk values). */
export function wrap(x: unknown): unknown {
	if (x === null || x === undefined) return 'Null'
	if (typeof x === 'number') return Number.isInteger(x) ? { Int: x } : { Real: x }
	if (typeof x === 'bigint') return { Int: Number(x) }
	if (typeof x === 'string') return { Text: x }
	if (x instanceof Uint8Array) return { Blob: Buffer.from(x).toString('base64') }
	throw new TypeError(`unsupported param type: ${typeof x}`)
}

/**
 * Build the outbox Insert op. seq uses the 'Lsn' placeholder so the envelope
 * assigns the monotonic sequence — an event exists iff its mutation committed.
 * Emit it in the SAME execute() call as the data ops.
 */
export function outboxOp(topic: string, event: string, payload: unknown): Op {
	return {
		Sql: {
			kind: 'Insert',
			table: 'outbox',
			pk_col: 'seq',
			columns: ['seq', 'topic', 'event', 'payload', 'created_at'],
			values: [
				'Lsn',
				v.text(topic),
				v.text(event),
				v.text(JSON.stringify(payload ?? null)),
				v.int(Date.now()),
			],
			pk: 'Lsn',
		},
	}
}

export interface OutboxEvent {
	seq: number
	topic: string
	event: string
	payload: unknown
	created_at: number
}

/**
 * GET /events handler factory. Auth: `x-events-token` header or Bearer token.
 * Returns {events, last_seq}; payload arrives parsed. last_seq is the newest
 * row in the table (not the newest returned), so empty polls still advance
 * waiters cheaply.
 */
export function eventsEndpoint(
	engine: EventsStore,
	opts: { token: string | ((req: Request) => boolean); limit?: number },
): (req: Request) => Promise<Response> {
	return async (req: Request) => {
		const authed = typeof opts.token === 'function'
			? opts.token(req)
			: (req.headers.get('x-events-token') ?? req.headers.get('authorization')?.replace(/^Bearer /, ''))
				=== opts.token
		if (!authed) return Response.json({ error: 'unauthorized' }, { status: 401 })

		const u = new URL(req.url)
		const since = Number.parseInt(u.searchParams.get('since') ?? '0', 10) || 0
		const topic = u.searchParams.get('topic')
		const limit = Math.min(Math.max(opts.limit ?? 500, 1), 5000)

		const params: unknown[] = [since]
		let sql = 'SELECT seq, topic, event, payload, created_at FROM outbox WHERE seq > ?'
		if (topic) {
			sql += ' AND topic = ?'
			params.push(topic)
		}
		sql += ` ORDER BY seq LIMIT ${limit}`

		const rows = JSON.parse(await engine.query(sql, JSON.stringify(params.map(wrap)))) as Array<{
			seq: number
			topic: string
			event: string
			payload: string | null
			created_at: number
		}>
		const events: OutboxEvent[] = rows.map((r) => ({
			seq: r.seq,
			topic: r.topic,
			event: r.event,
			payload: r.payload === null ? null : JSON.parse(r.payload),
			created_at: r.created_at,
		}))
		const lastRows = JSON.parse(
			await engine.query('SELECT COALESCE(MAX(seq), 0) AS n FROM outbox'),
		) as Array<{ n: number }>
		return Response.json({ events, last_seq: lastRows[0]?.n ?? 0 })
	}
}

export interface EventTx {
	ops: Op[]
}

export type ConsumerHandlers = Record<string, (ev: OutboxEvent, tx: EventTx) => Promise<void> | void>

export interface Consumer {
	start(): Promise<void>
	stop(): void
	/** Current committed cursor (what has been fully processed). */
	cursor(): Promise<number>
}

/**
 * Poller. Guarantees: the cursor Upsert shares the envelope write with the
 * handler's ops, so a crash either applied both or neither. Handlers must be
 * idempotent (redelivery is possible after a crash between polls). A throwing
 * handler aborts the batch — nothing advances, the same event re-arrives next
 * poll. Unknown events are skipped (cursor still advances).
 */
export function createConsumer(opts: {
	url: string
	token: string
	store: EventsStore
	handlers: ConsumerHandlers
	topic?: string
	cursorTopic?: string
	pollMs?: number
	log?: (msg: string) => void
}): Consumer {
	const ct = opts.cursorTopic ?? opts.topic ?? 'default'
	const pollMs = opts.pollMs ?? 300_000
	const log = opts.log ?? (() => {})
	let timer: ReturnType<typeof setInterval> | undefined
	let running = false
	let stopped = false

	const cursorSql = 'SELECT COALESCE(last_seq, 0) AS n FROM _events_cursor WHERE topic = ?'
	async function readCursor(): Promise<number> {
		const rows = JSON.parse(await opts.store.query(cursorSql, JSON.stringify([v.text(ct)]))) as Array<{
			n: number
		}>
		return rows[0]?.n ?? 0
	}

	async function tick(): Promise<void> {
		if (running) return
		running = true
		try {
			for (;;) {
				const since = await readCursor()
				const u = new URL(opts.url)
				u.searchParams.set('since', String(since))
				if (opts.topic) u.searchParams.set('topic', opts.topic)
				const res = await fetch(u, { headers: { 'x-events-token': opts.token } })
				if (!res.ok) {
					log(`events: poll failed ${res.status}`)
					return
				}
				const body = (await res.json()) as { events: OutboxEvent[] }
				for (const ev of body.events) {
					const tx: EventTx = { ops: [] }
					const h = opts.handlers[ev.event]
					if (h) await h(ev, tx)
					tx.ops.push({
						Sql: {
							kind: 'Upsert',
							table: '_events_cursor',
							pk_col: 'topic',
							columns: ['topic', 'last_seq'],
							values: [v.text(ct), v.int(ev.seq)],
							pk: v.text(ct),
						},
					})
					await opts.store.execute(JSON.stringify({ actor: 'events-consumer', ops: tx.ops }))
				}
				if (body.events.length === 0) return
			}
		} catch (err) {
			log(`events: tick error ${err instanceof Error ? err.message : String(err)}`)
		} finally {
			running = false
		}
	}

	return {
		async start() {
			if (!opts.store.bootstrap) throw new Error('store must expose bootstrap() for CURSOR_DDL')
			await opts.store.bootstrap(JSON.stringify([CURSOR_DDL]))
			await tick()
			if (stopped) return
			timer = setInterval(() => void tick(), pollMs)
		},
		stop() {
			stopped = true
			if (timer) clearInterval(timer)
		},
		cursor: readCursor,
	}
}
