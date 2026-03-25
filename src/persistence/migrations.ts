/**
 * @module persistence/migrations
 * SQLite migration runner. Applies numbered SQL files from migrations/ directory.
 */

import fs from 'fs';
import path from 'path';
import Database from 'better-sqlite3';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('migrations');
const MIGRATIONS_DIR = path.resolve(__dirname, '..', '..', 'migrations');

export interface MigrationInfo {
  version: number;
  filename: string;
  applied: boolean;
  applied_at?: number;
}

/**
 * Run all pending migrations on the given database.
 */
export function runMigrations(db: Database.Database): void {
  // Ensure schema_migrations table exists
  db.exec(`
    CREATE TABLE IF NOT EXISTS schema_migrations (
      version INTEGER PRIMARY KEY,
      applied_at INTEGER NOT NULL,
      filename TEXT NOT NULL
    )
  `);

  const applied = new Set(
    db.prepare('SELECT version FROM schema_migrations').all()
      .map((r: any) => r.version as number)
  );

  const migrationFiles = fs.readdirSync(MIGRATIONS_DIR)
    .filter(f => f.endsWith('.sql'))
    .sort();

  for (const filename of migrationFiles) {
    const versionMatch = filename.match(/^(\d+)/);
    if (!versionMatch) {
      log.warn(`Skipping migration file with no version prefix: ${filename}`);
      continue;
    }

    const version = parseInt(versionMatch[1], 10);
    if (applied.has(version)) {
      log.debug(`Migration ${version} already applied: ${filename}`);
      continue;
    }

    const sqlPath = path.join(MIGRATIONS_DIR, filename);
    const sql = fs.readFileSync(sqlPath, 'utf-8');

    log.info(`Applying migration ${version}: ${filename}`);

    const transaction = db.transaction(() => {
      db.exec(sql);
      db.prepare(
        'INSERT INTO schema_migrations (version, applied_at, filename) VALUES (?, ?, ?)'
      ).run(version, nowMs(), filename);
    });

    try {
      transaction();
      log.info(`Migration ${version} applied successfully`);
    } catch (err) {
      log.error(`Migration ${version} failed: ${err}`);
      throw err;
    }
  }
}

/**
 * Get list of all migrations and their status.
 */
export function getMigrationStatus(db: Database.Database): MigrationInfo[] {
  const applied = new Map<number, { applied_at: number }>();

  try {
    const rows = db.prepare('SELECT version, applied_at FROM schema_migrations').all() as any[];
    for (const r of rows) {
      applied.set(r.version, { applied_at: r.applied_at });
    }
  } catch {
    // Table doesn't exist yet
  }

  const migrationFiles = fs.readdirSync(MIGRATIONS_DIR)
    .filter(f => f.endsWith('.sql'))
    .sort();

  return migrationFiles.map(filename => {
    const versionMatch = filename.match(/^(\d+)/);
    const version = versionMatch ? parseInt(versionMatch[1], 10) : 0;
    const info = applied.get(version);
    return {
      version,
      filename,
      applied: !!info,
      applied_at: info?.applied_at,
    };
  });
}
