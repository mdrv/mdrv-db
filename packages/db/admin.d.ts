import type { AdminEngine, MdrvAdminOptions } from './admin'

export interface BackupVerifyReport {
	ok: boolean
	name: string | null
	checked: number
	bytes: number
	anomalies: string[]
}

export function createMdrvAdmin(
	engine: AdminEngine,
	opts: MdrvAdminOptions,
): (req: Request) => Promise<Response>

export function verifyBackupDir(dir: string): Promise<BackupVerifyReport>
