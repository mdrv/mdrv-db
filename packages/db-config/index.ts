/**
 * @mdrv/db-config — typed config + hooks layer.
 * Static fleet config lives in ~/.config/mdrv-db/config.toml (read by
 * @mdrv/db-maintenance); dynamic hooks live in hooks/<slug>.ts and run inside
 * each Bun app. The daemon never executes scripts.
 */
import { parse } from 'smol-toml'

export type Durability = 'per-write' | 'group-commit-10ms'

export interface BackupPolicy {
	cron: string
	retention_days: number
	mode?: 'online-or-offline' | 'online' | 'offline'
}

/** Circle membership = AND of label predicates. */
export interface CircleConfig {
	labels: Array<[string, string]>
	observers: string[]
}

export interface RpcConfig {
	/** Owner app's maintenance endpoint, e.g. http://127.0.0.1:3111/mdrv/rpc */
	url: string
	/** Env var holding the shared token (read by both owner and daemon) */
	token_env: string
}

export interface DbConfig {
	name?: string
	owner: string
	/** Absolute data dir override; default <sched.data_root>/<slug> */
	data_dir?: string
	/** Maintenance RPC on the owner app; absent = offline-only */
	rpc?: RpcConfig
	durability?: Durability
	backup?: BackupPolicy
	verify?: 'weekly' | 'daily' | 'off'
	mid?: {
		admins?: string[]
		session?: { access_ttl_min?: number; refresh_ttl_days?: number; max_per_user?: number }
		mid?: {
			min_len?: number
			max_len?: number
			rename_cooldown_days?: number
			renames_per_period?: number
			period_days?: number
			squat_days?: number
		}
		privacy?: { ip?: 'prefix' | 'none' }
		outbox?: { retention_days?: number }
		circle?: Record<string, CircleConfig>
	}
	[key: string]: unknown
}

export interface MdrvConfig {
	defaults?: { durability?: Durability; verify?: string }
	sched?: { data_root?: string; backup_root?: string }
	db: Record<string, DbConfig>
}

export interface MdrvEvent {
	seq: number
	type: string
	member_id: string
	payload: Record<string, unknown>
	created_at: number
}

export interface MdrvHooks {
	onEvent?: (evt: MdrvEvent, mdrv: unknown) => void | Promise<void>
}

export function loadConfig(text: string): MdrvConfig {
	return parse(text) as unknown as MdrvConfig
}
