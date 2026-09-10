/**
 * Smoke test for @mdrv/db-events. Run: bun packages/db-events/smoke.ts
 * Two real engines (producer + consumer over HTTP): proves atomic emit,
 * catch-up polling, atomic cursor, and no-advance on handler failure.
 */
import { MdrvDb } from '@mdrv/db'
import { rmSync } from 'node:fs'
import { createConsumer, CURSOR_DDL, eventsEndpoint, OUTBOX_DDL, outboxOp } from './index.ts'

let checks = 0
const ok = (name: string, cond: boolean) => {
	if (!cond) {
		console.error(`FAIL: ${name}`)
		process.exit(1)
	}
	checks++
	console.log(`ok ${checks} - ${name}`)
}

const dir = (n: string) => `/tmp/mdrv-db-events-smoke-${n}-${Date.now()}`
const P = dir('p')
const C = dir('c')
for (const d of [P, C]) rmSync(d, { recursive: true, force: true })

const producer = new MdrvDb(P, 'events-producer', true)
const consumer = new MdrvDb(C, 'events-consumer', true)
await producer.bootstrap(
	JSON.stringify([
		OUTBOX_DDL,
		'CREATE TABLE IF NOT EXISTS members (id INTEGER PRIMARY KEY, mid TEXT, name TEXT)',
	]),
)
await consumer.bootstrap(
	JSON.stringify([
		CURSOR_DDL,
		'CREATE TABLE IF NOT EXISTS roster (mid TEXT PRIMARY KEY, name TEXT)',
	]),
)

// 1. atomic emit: member insert + outbox event share ONE write
const r1 = JSON.parse(
	await producer.execute(
		JSON.stringify({
			actor: 'smoke',
			ops: [
				{
					Sql: {
						kind: 'Insert',
						table: 'members',
						pk_col: 'id',
						columns: ['id', 'mid', 'name'],
						values: ['Lsn', { Text: 'ua' }, { Text: 'Umar' }],
						pk: 'Lsn',
					},
				},
				outboxOp('mid', 'member_registered', { mid: 'ua', name: 'Umar' }),
			],
		}),
	),
) as { lsn: number; rows_changed: number }
ok('atomic emit (member + event one LSN)', r1.lsn > 0 && r1.rows_changed === 3)

// producer endpoint
const server = Bun.serve({ port: 0, fetch: eventsEndpoint(producer, { token: 'sekrit' }) })
const base = `http://127.0.0.1:${server.port}`

const noAuth = await fetch(`${base}/events`)
ok('endpoint 401 without token', noAuth.status === 401)
const empty = (await (await fetch(`${base}/events?since=999`, { headers: { 'x-events-token': 'sekrit' } })).json()) as {
	events: unknown[]
	last_seq: number
}
ok('empty poll + last_seq', empty.events.length === 0 && empty.last_seq === r1.lsn)

// 2. consumer processes; cursor advances in the SAME write as the mirror
let seen = 0
const consumerA = createConsumer({
	url: `${base}/events`,
	token: 'sekrit',
	store: consumer,
	handlers: {
		member_registered: (ev, tx) => {
			seen++
			tx.ops.push({
				Sql: {
					kind: 'Upsert',
					table: 'roster',
					pk_col: 'mid',
					columns: ['mid', 'name'],
					values: [{ Text: (ev.payload as { mid: string }).mid }, { Text: (ev.payload as { name: string }).name }],
					pk: { Text: (ev.payload as { mid: string }).mid },
				},
			})
		},
	},
	pollMs: 60_000,
})
await consumerA.start()
const roster = JSON.parse(await consumer.query('SELECT mid, name FROM roster')) as Array<{
	mid: string
	name: string
}>
const cur = await consumerA.cursor()
ok('consumer mirrored + cursor', roster.length === 1 && roster[0].name === 'Umar' && cur === r1.lsn)
await consumerA.stop()

// 3. emit more while consumer is down, then catch up
await producer.execute(
	JSON.stringify({
		actor: 'smoke',
		ops: [
			{
				Sql: {
					kind: 'Insert',
					table: 'members',
					pk_col: 'id',
					columns: ['id', 'mid', 'name'],
					values: ['Lsn', { Text: 'b2' }, { Text: 'Second' }],
					pk: 'Lsn',
				},
			},
			outboxOp('mid', 'member_registered', { mid: 'b2', name: 'Second' }),
		],
	}),
)
const consumerC = createConsumer({
	url: `${base}/events`,
	token: 'sekrit',
	store: consumer,
	handlers: {
		member_registered: (ev, tx) => {
			tx.ops.push({
				Sql: {
					kind: 'Upsert',
					table: 'roster',
					pk_col: 'mid',
					columns: ['mid', 'name'],
					values: [{ Text: (ev.payload as { mid: string }).mid }, { Text: (ev.payload as { name: string }).name }],
					pk: { Text: (ev.payload as { mid: string }).mid },
				},
			})
		},
	},
	pollMs: 60_000,
})
await consumerC.start()
const roster2 = JSON.parse(await consumer.query('SELECT COUNT(*) AS n FROM roster')) as Array<{ n: number }>
ok('catch-up after downtime', roster2[0].n === 2 && (await consumerC.cursor()) > cur)
await consumerC.stop()

// 4. failing handler: batch aborts, cursor does NOT advance, mirror NOT applied
await producer.execute(
	JSON.stringify({
		actor: 'smoke',
		ops: [
			{
				Sql: {
					kind: 'Insert',
					table: 'members',
					pk_col: 'id',
					columns: ['id', 'mid', 'name'],
					values: ['Lsn', { Text: 'c3' }, { Text: 'Third' }],
					pk: 'Lsn',
				},
			},
			outboxOp('mid', 'member_registered', { mid: 'c3', name: 'Third' }),
		],
	}),
)
const before = await consumerC.cursor()
const badConsumer = createConsumer({
	url: `${base}/events`,
	token: 'sekrit',
	store: consumer,
	handlers: {
		member_registered: () => {
			throw new Error('boom')
		},
	},
	pollMs: 60_000,
	log: () => {},
})
await badConsumer.start()
ok('failed handler: no advance, no mirror', (await badConsumer.cursor()) === before)
await badConsumer.stop()

const n3 = JSON.parse(await consumer.query('SELECT COUNT(*) AS n FROM roster')) as Array<{ n: number }>
ok('third member NOT mirrored while handler broken', n3[0].n === 2)

server.stop(true)
await producer.close()
await consumer.close()
for (const d of [P, C]) rmSync(d, { recursive: true, force: true })
console.log(`\n${checks} checks passed`)
