import type { AdminEngine, MdrvDbAdminOptions } from './admin'

export interface BackupVerifyReport {
	ok: boolean
	name: string | null
	checked: number
	bytes: number
	anomalies: string[]
}

export function createMdrvDbAdmin(
	engine: AdminEngine,
	opts: MdrvDbAdminOptions,
): (req: Request) => Promise<Response>

export function verifyBackupDir(dir: string): Promise<BackupVerifyReport>
